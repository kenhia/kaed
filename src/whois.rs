//! Tailnet cross-check: which node did this request come from?
//! (Sprint 023, korg WI 2392.)
//!
//! kaed authenticates by *declared name* (023 — see [`crate::server`]),
//! and a name says nothing about where it was declared from.
//! `tailscale whois` closes that gap: the caller's tailnet address
//! resolves to a node, which is recorded beside the declared author on
//! every journaled mutation and every secrets audit row, so "why did a
//! write from `claude` show up here" is answerable after the fact.
//!
//! **It is data, not a check.** A whois failure never refuses a request
//! — tailscaled being down, the binary being absent, the address being
//! unknown to the tailnet, and whois being switched off all resolve to
//! the same thing: `unknown`. The one place the answer can affect an
//! outcome is node pinning ([`crate::config::WhoisConfig::enforce`]),
//! which is off by default.
//!
//! ## Where the address comes from
//!
//! Measured on kai, 2026-09-12: kaed runs behind `tailscale serve`
//! (`https://kai.<tailnet>:4870` → `proxy http://localhost:4870`) and
//! binds `127.0.0.1:4870`, so the socket peer is always loopback and
//! carries no information. What `serve` forwards is `X-Forwarded-For:
//! <tailnet ip>`. So `X-Forwarded-For` is the primary source and the
//! socket peer is the fallback for a direct (non-`serve`) deployment.
//!
//! This is the same deployment shape klams measured for its own sprint
//! 049, which is why the rule transfers unchanged rather than being
//! re-derived here.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// What gets recorded when the node could not be determined — whois
/// disabled, no address, tailscaled down, or an address the tailnet does
/// not know. One word for all four on purpose: a reader of the journal
/// should not be invited to treat any of them as more trustworthy than
/// the others.
pub const UNKNOWN_NODE: &str = "unknown";

/// Resolves a tailnet address to a short node name (`kai`, `kubs0`).
///
/// A trait so the auth middleware can be tested without a tailnet, and
/// so a deployment with no `tailscale` binary can install nothing at all
/// rather than pay a failed process spawn per address.
#[async_trait::async_trait]
pub trait NodeResolver: Send + Sync + std::fmt::Debug {
    /// The node `addr` belongs to, or `None` if it cannot be resolved.
    async fn resolve(&self, addr: IpAddr) -> Option<String>;
}

/// Extract the short node name from `tailscale whois --json` output.
///
/// The short name is the first DNS label of `Node.Name`, which arrives
/// fully qualified and trailing-dotted (`kai.<tailnet>.`).
///
/// Returns `None` for anything that is not that — notably the literal
/// `peer not found` line whois prints for an address the tailnet does
/// not know, which is **not** JSON. "Did not parse" and "command failed"
/// are deliberately the same outcome: both mean the node is unknown, and
/// distinguishing them would only tempt a caller into treating one of
/// them as an error worth failing on.
#[must_use]
pub fn parse_node_name(stdout: &str) -> Option<String> {
    let parsed: serde_json::Value = serde_json::from_str(stdout).ok()?;
    let name = parsed.get("Node")?.get("Name")?.as_str()?;
    let short = name.split('.').next()?.trim();
    if short.is_empty() {
        return None;
    }
    Some(short.to_string())
}

/// The first entry of `X-Forwarded-For`, which is the original client in
/// the standard's ordering; kaed sits behind at most one proxy
/// (`tailscale serve`). Falls back to the socket peer for a direct
/// deployment, where there is no forwarding header to read.
#[must_use]
pub fn caller_addr(headers: &http::HeaderMap, peer: Option<IpAddr>) -> Option<IpAddr> {
    headers
        .get("x-forwarded-for")
        .and_then(|h| h.to_str().ok())
        .and_then(|v| v.split(',').next())
        .and_then(|v| v.trim().parse::<IpAddr>().ok())
        .or(peer)
}

/// The outcome of a cache lookup.
///
/// Three states, not two: absent-or-expired is a `Miss`, but a live
/// entry can itself say "this address does not resolve" — and that
/// negative answer is the one worth caching, because otherwise an
/// unresolvable peer spawns a process on every request.
#[derive(Debug, Clone, PartialEq, Eq)]
enum CacheLookup {
    Miss,
    Hit(Option<String>),
}

/// [`NodeResolver`] backed by the `tailscale` CLI, with a TTL cache.
#[derive(Debug)]
pub struct TailscaleWhois {
    binary: String,
    ttl: Duration,
    cache: Mutex<HashMap<IpAddr, (Instant, Option<String>)>>,
}

impl TailscaleWhois {
    /// As [`Self::new`], with an explicit binary path.
    #[must_use]
    pub fn with_binary(binary: impl Into<String>, ttl: Duration) -> Self {
        Self {
            binary: binary.into(),
            ttl,
            cache: Mutex::new(HashMap::new()),
        }
    }

    fn cached(&self, addr: IpAddr) -> CacheLookup {
        let cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let Some((at, value)) = cache.get(&addr) else {
            return CacheLookup::Miss;
        };
        if at.elapsed() > self.ttl {
            return CacheLookup::Miss;
        }
        CacheLookup::Hit(value.clone())
    }

    fn store(&self, addr: IpAddr, value: Option<String>) {
        let mut cache = self
            .cache
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        cache.insert(addr, (Instant::now(), value));
    }
}

#[async_trait::async_trait]
impl NodeResolver for TailscaleWhois {
    async fn resolve(&self, addr: IpAddr) -> Option<String> {
        if let CacheLookup::Hit(node) = self.cached(addr) {
            return node;
        }
        let output = tokio::process::Command::new(&self.binary)
            .arg("whois")
            .arg("--json")
            .arg(addr.to_string())
            .output()
            .await;
        let node = match output {
            Ok(out) => parse_node_name(&String::from_utf8_lossy(&out.stdout)),
            Err(e) => {
                // Debug, not warn: on a host with no tailscale this fires
                // once per address per TTL forever, and it is a supported
                // configuration (`[whois] enabled = false` is the way to
                // silence it deliberately).
                tracing::debug!(
                    error = %e,
                    binary = %self.binary,
                    "tailscale whois could not be invoked; recording node as unknown"
                );
                None
            }
        };
        self.store(addr, node.clone());
        node
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Fixtures measured against the live tailnet on 2026-09-12, cut down
    /// to the fields this parser reads. The tailnet suffix is a
    /// placeholder: this repo is public (sprint 003) and the real one is
    /// not committed, in files or commit messages.
    const TAGGED_NODE: &str = r#"{
      "Node": {
        "ID": 8503781158862512,
        "Name": "kai.example-tailnet.ts.net.",
        "Tags": ["tag:server"],
        "Hostinfo": { "Hostname": "kai" }
      },
      "UserProfile": { "LoginName": "tagged-devices" }
    }"#;

    const USER_DEVICE: &str = r#"{
      "Node": { "Name": "cleo.example-tailnet.ts.net.", "Tags": null },
      "UserProfile": { "LoginName": "someone@example.com" }
    }"#;

    #[test]
    fn parses_short_name_from_a_tagged_node() {
        assert_eq!(parse_node_name(TAGGED_NODE).as_deref(), Some("kai"));
    }

    /// A user-owned device has no `Tags` and a real `LoginName`; the node
    /// name is read the same way regardless, because the node — not the
    /// human — is the fact being recorded.
    #[test]
    fn parses_short_name_from_a_user_device() {
        assert_eq!(parse_node_name(USER_DEVICE).as_deref(), Some("cleo"));
    }

    /// The measured failure shape: whois prints `peer not found` on
    /// stdout, as a bare line, not as JSON.
    #[test]
    fn unknown_peer_output_is_not_an_error_it_is_unknown() {
        assert_eq!(parse_node_name("peer not found\n"), None);
    }

    #[test]
    fn malformed_or_empty_output_is_unknown() {
        for junk in ["", "   ", "{}", r#"{"Node":{}}"#, r#"{"Node":{"Name":""}}"#] {
            assert_eq!(parse_node_name(junk), None, "input {junk:?}");
        }
    }

    #[test]
    fn forwarded_for_beats_the_socket_peer() {
        let mut h = http::HeaderMap::new();
        h.insert("x-forwarded-for", "100.64.0.7".parse().unwrap());
        let peer: IpAddr = "127.0.0.1".parse().unwrap();
        assert_eq!(
            caller_addr(&h, Some(peer)),
            Some("100.64.0.7".parse().unwrap())
        );
    }

    /// Only the first entry: behind `tailscale serve` the original client
    /// is leftmost, and kaed sits behind at most one proxy.
    #[test]
    fn only_the_first_forwarded_entry_is_read() {
        let mut h = http::HeaderMap::new();
        h.insert("x-forwarded-for", "100.64.0.7, 100.64.0.1".parse().unwrap());
        assert_eq!(caller_addr(&h, None), Some("100.64.0.7".parse().unwrap()));
    }

    /// A direct (non-`serve`) deployment has no forwarding header, and
    /// the socket peer is the only thing there is.
    #[test]
    fn the_socket_peer_is_the_fallback() {
        let peer: IpAddr = "100.64.0.9".parse().unwrap();
        assert_eq!(caller_addr(&http::HeaderMap::new(), Some(peer)), Some(peer));
    }

    #[test]
    fn a_garbled_forwarded_header_is_no_address_not_a_panic() {
        let mut h = http::HeaderMap::new();
        h.insert("x-forwarded-for", "not-an-ip".parse().unwrap());
        assert_eq!(caller_addr(&h, None), None);
    }

    /// A miss is cached as a miss. Without this, an address the tailnet
    /// cannot resolve spawns a process on every single request from it.
    #[tokio::test]
    async fn negative_answers_are_cached_too() {
        // `true` exits 0 with empty stdout — an unparseable answer, so the
        // resolved value is `None`.
        let w = TailscaleWhois::with_binary("true", Duration::from_secs(60));
        let addr: IpAddr = "100.64.0.99".parse().unwrap();
        assert_eq!(w.resolve(addr).await, None);
        assert_eq!(
            w.cached(addr),
            CacheLookup::Hit(None),
            "a `None` answer must still occupy a cache slot, or every \
             request from an unresolvable peer spawns a process"
        );
    }

    /// A binary that does not exist resolves to `unknown` rather than
    /// erroring — the "no tailscale on this host" deployment.
    #[tokio::test]
    async fn a_missing_binary_resolves_to_unknown() {
        let w = TailscaleWhois::with_binary(
            "definitely-not-a-real-binary-kaed-023",
            Duration::from_secs(60),
        );
        assert_eq!(w.resolve("100.64.0.7".parse().unwrap()).await, None);
    }

    #[tokio::test]
    async fn expired_entries_are_not_served() {
        let w = TailscaleWhois::with_binary("true", Duration::from_millis(1));
        let addr: IpAddr = "100.64.0.99".parse().unwrap();
        w.store(addr, Some("stale".into()));
        tokio::time::sleep(Duration::from_millis(5)).await;
        assert_eq!(
            w.cached(addr),
            CacheLookup::Miss,
            "an expired entry must be a miss"
        );
    }
}
