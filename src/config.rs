//! TOML config: roots, auth token indirection, limits, journal.
//!
//! Tokens never live in the file — `[auth]` maps an author identity to the
//! env var holding its bearer token, resolved at startup. `resolve()` is
//! the validation gate: it canonicalizes roots, rejects duplicates, and
//! reads token env vars, producing the runtime view the server uses.

use anyhow::{Context, bail};
use serde::Deserialize;
use std::collections::BTreeMap;
use std::net::SocketAddr;
use std::path::{Path, PathBuf};

#[derive(Debug, Deserialize, Default)]
#[serde(deny_unknown_fields)]
pub struct Config {
    #[serde(default)]
    pub server: ServerConfig,
    #[serde(default)]
    pub roots: Vec<RootConfig>,
    /// author identity -> where its bearer token lives
    #[serde(default)]
    pub auth: BTreeMap<String, AuthEntry>,
    #[serde(default)]
    pub limits: Limits,
    #[serde(default)]
    pub journal: JournalConfig,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServerConfig {
    #[serde(default = "default_bind")]
    pub bind: SocketAddr,
    /// Extra `Host` header values to accept, on top of loopback — set this
    /// to the tailnet hostname when fronted by `tailscale serve`.
    #[serde(default)]
    pub allowed_hosts: Vec<String>,
}

impl Default for ServerConfig {
    fn default() -> Self {
        Self {
            bind: default_bind(),
            allowed_hosts: Vec::new(),
        }
    }
}

fn default_bind() -> SocketAddr {
    // homelab convention: same port on every host, loopback only
    "127.0.0.1:4870".parse().expect("valid default bind")
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RootConfig {
    pub name: String,
    pub path: String,
    pub description: Option<String>,
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthEntry {
    pub token_env: String,
}

#[derive(Debug, Clone, Copy, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Limits {
    #[serde(default = "default_max_read_bytes")]
    pub max_read_bytes: usize,
    #[serde(default = "default_max_file_bytes")]
    pub max_file_bytes: u64,
    #[serde(default = "default_search_max_results")]
    pub search_max_results: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            max_read_bytes: default_max_read_bytes(),
            max_file_bytes: default_max_file_bytes(),
            search_max_results: default_search_max_results(),
        }
    }
}

fn default_max_read_bytes() -> usize {
    262_144
}
fn default_max_file_bytes() -> u64 {
    8_388_608
}
fn default_search_max_results() -> usize {
    50
}

#[derive(Debug, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct JournalConfig {
    /// Defaults to `$XDG_DATA_HOME/kaed/journal.db` (or `~/.local/share/…`).
    pub path: Option<String>,
    #[serde(default = "default_retention_days")]
    pub retention_days: u32,
}

// manual impl: a derived Default would zero retention_days when the whole
// [journal] table is absent, bypassing the serde field default
impl Default for JournalConfig {
    fn default() -> Self {
        Self {
            path: None,
            retention_days: default_retention_days(),
        }
    }
}

fn default_retention_days() -> u32 {
    30
}

impl Config {
    pub fn default_path() -> PathBuf {
        base_dir("XDG_CONFIG_HOME", ".config").join("kaed/config.toml")
    }

    pub fn load(path: &Path) -> anyhow::Result<Config> {
        let text = std::fs::read_to_string(path)
            .with_context(|| format!("reading config {}", path.display()))?;
        toml::from_str(&text).with_context(|| format!("parsing config {}", path.display()))
    }

    /// Validate and produce the runtime view. Fails on bad roots or
    /// duplicate names; a missing token env var is a warning (the identity
    /// is skipped), because one host rarely defines every agent's token.
    pub fn resolve(&self) -> anyhow::Result<Resolved> {
        let mut roots = Vec::new();
        for r in &self.roots {
            if r.name.is_empty() || r.name.contains('/') {
                bail!("root name {:?} must be non-empty and slash-free", r.name);
            }
            if roots.iter().any(|x: &ResolvedRoot| x.name == r.name) {
                bail!("duplicate root name {:?}", r.name);
            }
            let expanded = expand_home(&r.path);
            let canonical = std::fs::canonicalize(&expanded).with_context(|| {
                format!("root {:?}: canonicalizing {}", r.name, expanded.display())
            })?;
            if !canonical.is_dir() {
                bail!(
                    "root {:?}: {} is not a directory",
                    r.name,
                    canonical.display()
                );
            }
            roots.push(ResolvedRoot {
                name: r.name.clone(),
                path: canonical,
                description: r.description.clone(),
            });
        }

        let mut identities = Vec::new();
        for (author, entry) in &self.auth {
            match std::env::var(&entry.token_env) {
                Ok(token) if !token.is_empty() => identities.push(Identity {
                    author: author.clone(),
                    token,
                }),
                _ => tracing::warn!(
                    author,
                    env = entry.token_env,
                    "token env var unset or empty; identity disabled"
                ),
            }
        }

        let journal_path = match &self.journal.path {
            Some(p) => expand_home(p),
            None => base_dir("XDG_DATA_HOME", ".local/share").join("kaed/journal.db"),
        };

        Ok(Resolved {
            bind: self.server.bind,
            allowed_hosts: self.server.allowed_hosts.clone(),
            roots,
            identities,
            limits: self.limits,
            journal_path,
            journal_retention_days: self.journal.retention_days,
        })
    }
}

/// The validated runtime view of the config.
#[derive(Debug)]
pub struct Resolved {
    pub bind: SocketAddr,
    pub allowed_hosts: Vec<String>,
    pub roots: Vec<ResolvedRoot>,
    pub identities: Vec<Identity>,
    pub limits: Limits,
    pub journal_path: PathBuf,
    pub journal_retention_days: u32,
}

#[derive(Debug, Clone)]
pub struct ResolvedRoot {
    pub name: String,
    /// Canonicalized; the jail boundary for every path under this root.
    pub path: PathBuf,
    pub description: Option<String>,
}

#[derive(Debug, Clone)]
pub struct Identity {
    pub author: String,
    pub token: String,
}

fn base_dir(xdg_var: &str, home_fallback: &str) -> PathBuf {
    if let Ok(dir) = std::env::var(xdg_var)
        && !dir.is_empty()
    {
        return PathBuf::from(dir);
    }
    home().join(home_fallback)
}

fn home() -> PathBuf {
    PathBuf::from(std::env::var("HOME").expect("HOME is set"))
}

fn expand_home(path: &str) -> PathBuf {
    match path.strip_prefix("~/") {
        Some(rest) => home().join(rest),
        None => PathBuf::from(path),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn minimal_config_gets_defaults() {
        let cfg: Config = toml::from_str("").unwrap();
        assert_eq!(cfg.server.bind, default_bind());
        assert_eq!(cfg.limits.max_read_bytes, 262_144);
        assert_eq!(cfg.limits.max_file_bytes, 8_388_608);
        assert_eq!(cfg.limits.search_max_results, 50);
        assert_eq!(cfg.journal.retention_days, 30);
        assert!(cfg.roots.is_empty());
    }

    #[test]
    fn full_config_parses() {
        let cfg: Config = toml::from_str(
            r#"
            [server]
            bind = "127.0.0.1:4999"

            [[roots]]
            name = "home"
            path = "/home/ken"
            description = "everything under ~"

            [auth]
            claude = { token_env = "KAED_TOKEN_CLAUDE" }

            [limits]
            max_read_bytes = 1024

            [journal]
            retention_days = 7
            "#,
        )
        .unwrap();
        assert_eq!(cfg.server.bind.port(), 4999);
        assert_eq!(cfg.roots[0].name, "home");
        assert_eq!(cfg.auth["claude"].token_env, "KAED_TOKEN_CLAUDE");
        assert_eq!(cfg.limits.max_read_bytes, 1024);
        // unspecified limits keep their defaults
        assert_eq!(cfg.limits.max_file_bytes, 8_388_608);
        assert_eq!(cfg.journal.retention_days, 7);
    }

    #[test]
    fn unknown_keys_are_rejected() {
        assert!(toml::from_str::<Config>("[server]\nbindd = \"x\"").is_err());
    }

    #[test]
    fn resolve_rejects_duplicate_root_names() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().to_str().unwrap();
        let cfg: Config = toml::from_str(&format!(
            "[[roots]]\nname = \"a\"\npath = \"{p}\"\n[[roots]]\nname = \"a\"\npath = \"{p}\"\n"
        ))
        .unwrap();
        let err = cfg.resolve().unwrap_err();
        assert!(err.to_string().contains("duplicate root name"));
    }

    #[test]
    fn resolve_rejects_missing_root() {
        let cfg: Config =
            toml::from_str("[[roots]]\nname = \"a\"\npath = \"/nonexistent/kaed-test\"\n").unwrap();
        assert!(cfg.resolve().is_err());
    }

    #[test]
    fn resolve_canonicalizes_roots_and_resolves_tokens() {
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().to_str().unwrap();
        // unique env var per test: parallel tests share the process env
        unsafe { std::env::set_var("KAED_TEST_TOKEN_RESOLVE", "sekrit") };
        let cfg: Config = toml::from_str(&format!(
            r#"
            [[roots]]
            name = "t"
            path = "{p}"
            [auth]
            claude = {{ token_env = "KAED_TEST_TOKEN_RESOLVE" }}
            ghost = {{ token_env = "KAED_TEST_TOKEN_UNSET_XYZ" }}
            "#
        ))
        .unwrap();
        let resolved = cfg.resolve().unwrap();
        assert!(resolved.roots[0].path.is_absolute());
        assert_eq!(resolved.identities.len(), 1);
        assert_eq!(resolved.identities[0].author, "claude");
        assert_eq!(resolved.identities[0].token, "sekrit");
    }
}
