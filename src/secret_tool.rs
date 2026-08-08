//! The secret lifecycle (#1051) and the cross-session handle (#1052).
//!
//! Four verbs that never return a value — `describe`, `generate`,
//! `rotate`, `occurrences` — and one that only ever returns a value,
//! `reveal`, kept apart at the *tool* level because harness permissioning
//! is per-tool: `kaed__secret` gets allowlisted, `kaed__secret_reveal`
//! keeps prompting. That split is the load-bearing safety property here.
//!
//! Writes ride the existing txn engine, so base versions, atomicity, the
//! vanish guard, redacted diffs and journaling all apply unchanged. Every
//! lifecycle action lands in the `secret_events` audit stream — events and
//! digests, never payloads — which is what makes "has any agent ever seen
//! this secret?" answerable (D-6).
//!
//! The handle (#1052, PD-3) is `location + digest` with **no second
//! store**: persistence lives in the file the secret is already in, the
//! digest is host-independent BLAKE3 (PD-2), and `describe` is
//! `load_secret` — one read answers both (D-2).

use crate::config::{Limits, ResolvedRoot, ResolvedSecrets};
use crate::dotenv;
use crate::errors::{KaedError, Result};
use crate::fsops::{self, Secrecy};
use crate::journal::{Journal, SecretEvent};
use crate::search;
use crate::secrets;
use crate::shapes::{self, Shape};
use crate::txn::{self, BaseVersion, EditOp, EditRequest, FileChange};
use schemars::JsonSchema;
use serde::{Deserialize, Serialize};
use zeroize::Zeroizing;

/// Everything a lifecycle action needs from the server.
pub struct Ctx<'a> {
    /// The addressed root.
    pub root: &'a ResolvedRoot,
    /// Every local root — `occurrences` scans across them.
    pub roots: &'a [ResolvedRoot],
    pub limits: &'a Limits,
    pub author: &'a str,
    pub journal: &'a Journal,
    pub secrets: &'a ResolvedSecrets,
}

/// The PD-3 reference: host-qualified location plus content-address
/// digest. Carried in prose handoffs; consumed by `edit`'s `value_from`.
#[derive(Debug, Serialize, JsonSchema)]
pub struct Handle {
    /// Host-qualified root name, e.g. `kai:src`.
    pub root: String,
    pub path: String,
    pub key: String,
    /// Withheld below the entropy floor (PD-2): equality and staleness are
    /// then deliberately unanswerable.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
}

fn handle_line(h: &Handle) -> String {
    match &h.digest {
        Some(d) => format!("{}/{}#{}@{}", h.root, h.path, h.key, d),
        None => format!("{}/{}#{}", h.root, h.path, h.key),
    }
}

// -------------------------------------------------------------- `describe`

#[derive(Debug, Serialize, JsonSchema)]
pub struct DescribeResult {
    pub key: String,
    /// Disclosable shape (`hex`, `uuid`, `base64`, `prefixed:<tag>`,
    /// `text`, `empty`) — the non-random parts only.
    pub shape: String,
    /// Character count of the value.
    pub len: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// The sealed placeholder, as a redacted read would serve it.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// The cross-session reference (PD-3). Carry THIS in a handoff — the
    /// location plus digest — never the value.
    pub handle: Handle,
    /// The handle as one paste-able line:
    /// `kai:src/tools/korg/.env#KLAMS_TOKEN@a3f9c2d41b7e5860`.
    pub handle_line: String,
    /// Whether `rotate` could re-mint this value without an explicit shape.
    pub rotatable_by_detection: bool,
}

/// `describe` — the read half of the lifecycle, and `load_secret` (#1052)
/// in the same breath (D-2): shape metadata plus the durable handle.
/// Disclosure-free, so not journaled; the digest is already served by any
/// redacted read.
pub fn describe(ctx: &Ctx<'_>, path: &str, key: &str) -> Result<DescribeResult> {
    let (_, entry) = load_entry(ctx, path, key)?;
    let digest = secrets::clears_floor(&entry.value).then(|| secrets::digest_of(&entry.value));
    let handle = Handle {
        root: ctx.root.name.clone(),
        path: path.to_owned(),
        key: key.to_owned(),
        digest: digest.clone(),
    };
    Ok(DescribeResult {
        key: key.to_owned(),
        shape: secrets::shape_of(&entry.value),
        len: entry.value.chars().count(),
        digest,
        placeholder: (!entry.value.is_empty()).then(|| secrets::placeholder(key, &entry.value)),
        handle_line: handle_line(&handle),
        handle,
        rotatable_by_detection: shapes::detect(&entry.value).is_some(),
    })
}

// -------------------------------------------------------------- `generate`

#[derive(Debug, Serialize, JsonSchema)]
pub struct GenerateResult {
    pub txn_id: Option<i64>,
    pub key: String,
    /// The spec that was minted, normalized.
    pub shape: String,
    /// The sealed placeholder for the value just written. The agent never
    /// holds a secret it created.
    pub placeholder: String,
    pub handle: Handle,
    pub handle_line: String,
    pub files: Vec<FileChange>,
    /// Redacted proof of what changed, from the txn engine.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub struct GenerateParams<'a> {
    pub path: &'a str,
    pub key: &'a str,
    /// The file's current version (R2: every mutation declares its base).
    pub version: &'a str,
    /// A named entry from `[secrets] shapes`, or a raw spec.
    pub shape: &'a str,
    pub comment: Option<&'a str>,
    pub intent: Option<&'a str>,
}

/// `generate` — mint server-side, write through the txn engine, return
/// only the placeholder. New keys only: overwriting an existing value is
/// `rotate`'s job, and keeping the verbs disjoint keeps "this destroys a
/// value" attached to the verb that means it (D-4).
pub fn generate(ctx: &Ctx<'_>, p: &GenerateParams<'_>) -> Result<GenerateResult> {
    let shape = resolve_shape(ctx, p.shape)?;
    let (_, file) = load_file(ctx, p.path)?;
    if file.get(p.key).is_some() {
        return Err(KaedError::invalid_input(format!(
            "{}: key {:?} already exists — `generate` only mints new keys; use \
             action \"rotate\" to replace its value (same shape, new entropy), or \
             env_delete + drop_keys first if you mean to change its shape",
            p.path, p.key
        )));
    }

    let value = Zeroizing::new(shape.mint()?);
    let outcome = txn::apply(
        ctx.root,
        &EditRequest {
            base: vec![BaseVersion {
                path: p.path.to_owned(),
                version: p.version.to_owned(),
            }],
            ops: vec![EditOp::EnvSet {
                path: p.path.to_owned(),
                key: p.key.to_owned(),
                value: Some(value.to_string()),
                value_from: None,
                comment: p.comment.map(str::to_owned),
            }],
            dry_run: false,
            return_diff: true,
            intent: Some(
                p.intent
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("generate {} ({})", p.key, shape)),
            ),
            drop_keys: Vec::new(),
            allow_secrets: Vec::new(),
        },
        ctx.limits,
        ctx.author,
        ctx.journal,
    )?;

    let digest = secrets::clears_floor(&value).then(|| secrets::digest_of(&value));
    ctx.journal.add_secret_event(&SecretEvent {
        author: ctx.author,
        action: "generate",
        root: &ctx.root.name,
        path: p.path,
        key: p.key,
        old_digest: None,
        new_digest: digest.as_deref(),
        disclosed: false,
        destination: None,
        txn_id: outcome.txn_id,
        intent: p.intent.map(redact).as_deref(),
    })?;

    let handle = Handle {
        root: ctx.root.name.clone(),
        path: p.path.to_owned(),
        key: p.key.to_owned(),
        digest,
    };
    Ok(GenerateResult {
        txn_id: outcome.txn_id,
        key: p.key.to_owned(),
        shape: shape.to_string(),
        placeholder: secrets::placeholder(p.key, &value),
        handle_line: handle_line(&handle),
        handle,
        files: outcome.files,
        diff: outcome.diff,
        warnings: outcome.warnings,
    })
}

// ---------------------------------------------------------------- `rotate`

/// One extra location `rotate` writes the new value to. The feeder is
/// `occurrences`; each target declares its own base version (R2).
#[derive(Debug, Clone, Deserialize, JsonSchema)]
#[serde(deny_unknown_fields)]
pub struct AlsoTarget {
    /// Defaults to the primary's root. May name another host's root: the
    /// gateway then writes it via peer mode (PD-3's rotate-both-places).
    pub root: Option<String>,
    pub path: String,
    /// Defaults to the primary's key.
    pub key: Option<String>,
    /// That file's current version.
    pub version: String,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RotatedTarget {
    pub root: String,
    pub path: String,
    pub key: String,
    pub applied: bool,
    /// Why, when `applied` is false (remote targets fail independently —
    /// cross-host rotation is not atomic, and says so).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error: Option<serde_json::Value>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct RotateResult {
    /// The local transaction covering the primary and every local `also`
    /// target — one atomic write.
    pub txn_id: Option<i64>,
    pub key: String,
    pub shape: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub old_digest: Option<String>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub new_digest: Option<String>,
    pub placeholder: String,
    pub handle: Handle,
    pub handle_line: String,
    /// Every location written (or attempted): primary first, then `also`
    /// in request order. Local targets share `txn_id`; remote targets are
    /// separate transactions on their own hosts, journaled there.
    pub targets: Vec<RotatedTarget>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub diff: Option<String>,
    #[serde(skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

pub struct RotateParams<'a> {
    pub path: &'a str,
    pub key: &'a str,
    pub version: &'a str,
    /// Explicit shape (named or spec). Without it the shape is detected
    /// from the current value, and an undetectable value refuses (D-4).
    pub shape: Option<&'a str>,
    /// Local `also` targets only — the server routes remote ones.
    pub also: &'a [AlsoTarget],
    pub intent: Option<&'a str>,
}

/// The local half of `rotate`: mint once, write the primary and every
/// local `also` target in ONE transaction, journal one audit event per
/// location. Returns the minted value so the server can propagate it to
/// remote targets (PD-3's rotate-both-places path) — the value goes no
/// further than that caller and is zeroized when dropped.
pub fn rotate_local(
    ctx: &Ctx<'_>,
    p: &RotateParams<'_>,
) -> Result<(RotateResult, Zeroizing<String>)> {
    let (_, entry) = load_entry(ctx, p.path, p.key)?;
    if entry.value.is_empty() {
        return Err(KaedError::invalid_input(format!(
            "{}: key {:?} is empty — nothing to rotate; use `generate`",
            p.path, p.key
        )));
    }
    let shape = match p.shape {
        Some(s) => resolve_shape(ctx, s)?,
        None => shapes::detect(&entry.value).ok_or_else(|| {
            KaedError::invalid_input(format!(
                "{}: the current value of {:?} has no detectable shape ({}) — kaed \
                 will not guess what to mint. Pass `shape` explicitly (a [secrets] \
                 shapes name or a spec like hex(64)); for externally-issued tokens \
                 rotation happens at the provider, and kaed can only write the new \
                 value you were given (env_set with drop_keys)",
                p.path,
                p.key,
                secrets::shape_of(&entry.value)
            ))
        })?,
    };
    let old_digest = secrets::clears_floor(&entry.value).then(|| secrets::digest_of(&entry.value));
    let value = Zeroizing::new(shape.mint()?);
    let new_digest = secrets::clears_floor(&value).then(|| secrets::digest_of(&value));

    // Primary plus local also-targets, one atomic transaction. Rotation is
    // declared destruction — the verb *means* "replace the old value" — so
    // the rotated keys ride drop_keys internally (D-4).
    let mut base = vec![BaseVersion {
        path: p.path.to_owned(),
        version: p.version.to_owned(),
    }];
    let mut ops = vec![EditOp::EnvSet {
        path: p.path.to_owned(),
        key: p.key.to_owned(),
        value: Some(value.to_string()),
        value_from: None,
        comment: None,
    }];
    let mut drop_keys = vec![p.key.to_owned()];
    let mut targets = vec![RotatedTarget {
        root: ctx.root.name.clone(),
        path: p.path.to_owned(),
        key: p.key.to_owned(),
        applied: true,
        error: None,
    }];
    for t in p.also {
        if t.root.as_deref().is_some_and(|r| r != ctx.root.name) {
            return Err(KaedError::invalid_input(format!(
                "also-target {:?} names root {:?}: rotate_local only takes targets \
                 on the addressed root — the server routes the rest",
                t.path,
                t.root.as_deref().unwrap_or_default()
            )));
        }
        let key = t.key.as_deref().unwrap_or(p.key);
        if base.iter().any(|b| b.path == t.path) {
            // second key in the same file: version already declared
            if base
                .iter()
                .any(|b| b.path == t.path && b.version != t.version)
            {
                return Err(KaedError::invalid_input(format!(
                    "also-target {}: version disagrees with an earlier declaration \
                     of the same file",
                    t.path
                )));
            }
        } else {
            base.push(BaseVersion {
                path: t.path.clone(),
                version: t.version.clone(),
            });
        }
        ops.push(EditOp::EnvSet {
            path: t.path.clone(),
            key: key.to_owned(),
            value: Some(value.to_string()),
            value_from: None,
            comment: None,
        });
        drop_keys.push(key.to_owned());
        targets.push(RotatedTarget {
            root: ctx.root.name.clone(),
            path: t.path.clone(),
            key: key.to_owned(),
            applied: true,
            error: None,
        });
    }
    drop_keys.sort();
    drop_keys.dedup();

    let outcome = txn::apply(
        ctx.root,
        &EditRequest {
            base,
            ops,
            dry_run: false,
            return_diff: true,
            intent: Some(
                p.intent
                    .map(str::to_owned)
                    .unwrap_or_else(|| format!("rotate {} ({})", p.key, shape)),
            ),
            drop_keys,
            allow_secrets: Vec::new(),
        },
        ctx.limits,
        ctx.author,
        ctx.journal,
    )?;

    for t in &targets {
        ctx.journal.add_secret_event(&SecretEvent {
            author: ctx.author,
            action: "rotate",
            root: &t.root,
            path: &t.path,
            key: &t.key,
            old_digest: old_digest.as_deref(),
            new_digest: new_digest.as_deref(),
            disclosed: false,
            destination: None,
            txn_id: outcome.txn_id,
            intent: p.intent.map(redact).as_deref(),
        })?;
    }

    let handle = Handle {
        root: ctx.root.name.clone(),
        path: p.path.to_owned(),
        key: p.key.to_owned(),
        digest: new_digest.clone(),
    };
    Ok((
        RotateResult {
            txn_id: outcome.txn_id,
            key: p.key.to_owned(),
            shape: shape.to_string(),
            old_digest,
            new_digest,
            placeholder: secrets::placeholder(p.key, &value),
            handle_line: handle_line(&handle),
            handle,
            targets,
            diff: outcome.diff,
            warnings: outcome.warnings,
        },
        value,
    ))
}

// ----------------------------------------------------------- `occurrences`

#[derive(Debug, Serialize, JsonSchema)]
pub struct Occurrence {
    pub root: String,
    pub path: String,
    pub line: usize,
    /// The key holding the same value there (parsed from the placeholder).
    #[serde(skip_serializing_if = "Option::is_none")]
    pub key: Option<String>,
    /// True for the entry the query named.
    pub is_source: bool,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct OccurrencesResult {
    pub digest: String,
    pub occurrences: Vec<Occurrence>,
    /// Files searched across all local roots — the #1066 honesty number.
    pub files_searched: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub denied_hidden: usize,
    #[serde(skip_serializing_if = "is_zero")]
    pub classified_hidden: usize,
    /// What this scan structurally cannot see, stated rather than implied.
    pub coverage_note: String,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

/// `occurrences` — every classified-dotenv entry on THIS host holding the
/// same value, by digest equality over the redacted renderings (the same
/// surface `search` reads, D-8 of 008). Feeds `rotate.also`. The fleet
/// version is deliberately not rebuilt here: `search` with the digest and
/// a root pattern (`*:*`) has answered it since 010 (D-8).
pub fn occurrences(ctx: &Ctx<'_>, path: &str, key: &str) -> Result<OccurrencesResult> {
    let (_, entry) = load_entry(ctx, path, key)?;
    if !secrets::clears_floor(&entry.value) {
        return Err(KaedError::invalid_input(format!(
            "{path}: the value of {key:?} sits below the entropy floor, so its digest \
             is withheld (PD-2) and equality across files is deliberately not \
             answerable — a disclosed digest of a small value space would be an \
             offline enumeration oracle"
        )));
    }
    let digest = secrets::digest_of(&entry.value);

    let mut result = OccurrencesResult {
        digest: digest.clone(),
        occurrences: Vec::new(),
        files_searched: 0,
        denied_hidden: 0,
        classified_hidden: 0,
        coverage_note: format!(
            "digest equality is only visible where redaction runs: classified dotenv \
             files on this host. A plaintext copy in an unclassified file contains the \
             value, not the digest, and is invisible here (write-side leak detection is \
             korg #1053). For the fleet, `search` with pattern {digest:?} and root \
             pattern \"*:*\" asks every reachable host in one call."
        ),
    };
    // The digest only ever appears inside placeholders of the redacted
    // rendering, so a literal search for it finds exactly the classified
    // dotenv entries sealing the same value.
    for root in ctx.roots {
        let found = search::search(
            root,
            &search::SearchParams {
                pattern: &digest,
                regex: false,
                glob: None,
                path: "",
                context: 0,
                max_results: 500,
            },
            ctx.limits,
        )?;
        result.files_searched += found.files_searched;
        result.denied_hidden += found.denied_hidden;
        result.classified_hidden += found.classified_hidden;
        for m in found.matches {
            let matched_key = m
                .text
                .split(secrets::PLACEHOLDER_OPEN)
                .nth(1)
                .and_then(|rest| rest.split('@').next())
                .filter(|k| !k.is_empty())
                .map(str::to_owned);
            let is_source =
                root.name == ctx.root.name && m.path == path && matched_key.as_deref() == Some(key);
            result.occurrences.push(Occurrence {
                root: root.name.clone(),
                path: m.path,
                line: m.line,
                key: matched_key,
                is_source,
            });
        }
    }
    Ok(result)
}

// ---------------------------------------------------------------- `reveal`

#[derive(Debug, Serialize, JsonSchema)]
pub struct RevealResult {
    pub key: String,
    /// The plaintext. The one field in the whole surface that carries one.
    pub value: String,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub digest: Option<String>,
    /// Always true — and the agent is expected to surface the disclosure
    /// to the human it is working for, not just consume the value.
    pub disclosed: bool,
    pub note: String,
}

pub struct RevealParams<'a> {
    pub path: &'a str,
    pub key: &'a str,
    /// Required. The only kaed tool where it is: a disclosure with no
    /// recorded reason is the row the audit stream exists to prevent (D-1).
    pub intent: &'a str,
    /// Exact-value semantics (PD-3): when given, a changed value refuses
    /// loudly instead of revealing whatever is current.
    pub expected_digest: Option<&'a str>,
    /// Set by kaed itself when this reveal is the source half of a
    /// cross-host `value_from` (D-5): the event journals as `transport`
    /// with this claimed destination. Callers passing it directly are
    /// making the same claim, and the value is disclosed either way.
    pub transport_destination: Option<&'a str>,
}

/// `secret_reveal` — the escape hatch, one key at a time, always
/// journaled. Kept behind its own tool name so the harness prompts for it
/// separately; kept minimal because the 008 measurement found zero live
/// pressure for plaintext (D-1).
pub fn reveal(ctx: &Ctx<'_>, p: &RevealParams<'_>) -> Result<RevealResult> {
    if !ctx.secrets.allow_reveal {
        return Err(KaedError::refused(
            p.path,
            "[secrets] allow_reveal = false",
            crate::errors::RefusalReason::RevealDisabled,
            "this host's config refuses secret_reveal entirely. The lifecycle verbs \
             (describe/generate/rotate) and value_from writes still work — they never \
             need plaintext. If a human decides this host should reveal, flip \
             allow_reveal in config.toml",
        ));
    }
    if p.intent.trim().is_empty() {
        return Err(KaedError::invalid_input(
            "secret_reveal requires a non-empty `intent`: the audit row must say why \
             the value was disclosed",
        ));
    }
    let (loaded_classified, entry) = load_entry(ctx, p.path, p.key)?;
    if !loaded_classified {
        return Err(KaedError::invalid_input(format!(
            "{}: not a classified file — read it directly; reveal exists only for \
             values kaed serves redacted",
            p.path
        )));
    }
    let digest = secrets::clears_floor(&entry.value).then(|| secrets::digest_of(&entry.value));
    if let Some(expected) = p.expected_digest {
        let actual = secrets::digest_of(&entry.value);
        if actual != expected {
            return Err(KaedError::invalid_input(format!(
                "{}: digest mismatch for {:?} — the value changed since that handle \
                 was taken (expected {expected}). Exact-vs-current must never be \
                 ambiguous: re-describe the key and decide whether current is what \
                 you want",
                p.path, p.key
            ))
            .with_data(serde_json::json!({
                "reason": "digest_mismatch",
                "expected_digest": expected,
                "actual_digest": digest,
            })));
        }
    }

    let action = if p.transport_destination.is_some() {
        "transport"
    } else {
        "reveal"
    };
    ctx.journal.add_secret_event(&SecretEvent {
        author: ctx.author,
        action,
        root: &ctx.root.name,
        path: p.path,
        key: p.key,
        old_digest: None,
        new_digest: digest.as_deref(),
        disclosed: true,
        destination: p.transport_destination,
        txn_id: None,
        intent: Some(redact(p.intent)).as_deref(),
    })?;

    Ok(RevealResult {
        key: p.key.to_owned(),
        value: entry.value,
        digest,
        disclosed: true,
        note: "this value has been disclosed into your context and the disclosure is \
               journaled; surface it to the human you are working for, and do not \
               write it anywhere kaed could have written it for you (env_set takes \
               placeholders and value_from handles)"
            .to_string(),
    })
}

// ------------------------------------------------------------------ shared

/// Load `path` as dotenv through every policy layer. Returns whether the
/// file is classified (reveal cares) and the parse.
fn load_file(ctx: &Ctx<'_>, path: &str) -> Result<(bool, dotenv::DotenvFile)> {
    let loaded = fsops::load_text(ctx.root, path, ctx.limits)?;
    match loaded.secrecy {
        Secrecy::ClassifiedDotenv { file, .. } => Ok((true, file)),
        Secrecy::Plain => {
            let file = dotenv::parse(&loaded.content).ok_or_else(|| {
                KaedError::invalid_input(format!(
                    "{path}: not dotenv-shaped (every line must be blank, a # comment, \
                     or KEY=value at column 0) — the secret lifecycle works on dotenv \
                     files"
                ))
            })?;
            Ok((false, file))
        }
    }
}

/// `load_file` plus one entry, cloned out of the buffer.
fn load_entry(ctx: &Ctx<'_>, path: &str, key: &str) -> Result<(bool, dotenv::Entry)> {
    let (classified, file) = load_file(ctx, path)?;
    let entry = file.get(key).ok_or_else(|| {
        KaedError::not_found(format!(
            "{path}: no key {key:?} — a redacted read of the file lists what is there"
        ))
        .with_data(serde_json::json!({ "reason": "no_such_key", "path": path, "key": key }))
    })?;
    Ok((classified, entry.clone()))
}

fn resolve_shape(ctx: &Ctx<'_>, spec_or_name: &str) -> Result<Shape> {
    if let Some(named) = ctx.secrets.shapes.get(spec_or_name) {
        return Ok(named.clone());
    }
    shapes::parse_spec(spec_or_name).map_err(|e| {
        if ctx.secrets.shapes.is_empty() {
            e
        } else {
            let names: Vec<&str> = ctx.secrets.shapes.keys().map(String::as_str).collect();
            KaedError::new(
                e.code,
                format!("{} (named shapes on this host: {names:?})", e.message),
            )
        }
    })
}

fn redact(s: &str) -> String {
    secrets::redact_free_text(s)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::history::RecordKind;
    use crate::journal::HistoryFilter;

    const TOKEN: &str = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60";

    struct Fixture {
        _dir: tempfile::TempDir,
        roots: Vec<ResolvedRoot>,
        limits: Limits,
        journal: Journal,
        secrets: ResolvedSecrets,
    }

    impl Fixture {
        fn new() -> Fixture {
            let dir = tempfile::tempdir().unwrap();
            let base = dir.path().canonicalize().unwrap();
            std::fs::write(
                base.join(".env"),
                format!("# service credentials\nKLAMS_TOKEN={TOKEN}\nDEBUG=true\nEMPTY=\n"),
            )
            .unwrap();
            let root = ResolvedRoot::with_default_classify("t:main", base);
            Fixture {
                _dir: dir,
                roots: vec![root],
                limits: Limits::default(),
                journal: Journal::open_in_memory().unwrap(),
                secrets: ResolvedSecrets::default(),
            }
        }

        fn ctx(&self) -> Ctx<'_> {
            Ctx {
                root: &self.roots[0],
                roots: &self.roots,
                limits: &self.limits,
                author: "claude",
                journal: &self.journal,
                secrets: &self.secrets,
            }
        }

        fn disk(&self, rel: &str) -> String {
            std::fs::read_to_string(self.roots[0].path.join(rel)).unwrap()
        }

        fn version(&self, rel: &str) -> String {
            fsops::version_of(self.disk(rel).as_bytes())
        }

        fn events(&self) -> Vec<crate::journal::SecretEventRow> {
            self.journal
                .secret_events(&HistoryFilter {
                    limit: 100,
                    ..Default::default()
                })
                .unwrap()
        }
    }

    #[test]
    fn describe_returns_the_handle_and_never_the_value() {
        let f = Fixture::new();
        let out = describe(&f.ctx(), ".env", "KLAMS_TOKEN").unwrap();
        assert_eq!(out.shape, "hex");
        assert_eq!(out.len, 32);
        let digest = secrets::digest_of(TOKEN);
        assert_eq!(out.digest.as_deref(), Some(digest.as_str()));
        assert_eq!(out.handle.root, "t:main");
        assert_eq!(out.handle_line, format!("t:main/.env#KLAMS_TOKEN@{digest}"));
        assert!(out.rotatable_by_detection);
        let json = serde_json::to_string(&out).unwrap();
        assert!(!json.contains(TOKEN), "describe leaked the value: {json}");
        // reads are disclosure-free: nothing lands in the audit stream
        assert!(f.events().is_empty());

        // below the floor: no digest, and the handle says so structurally
        let debug = describe(&f.ctx(), ".env", "DEBUG").unwrap();
        assert_eq!(debug.digest, None);
        assert!(!debug.handle_line.contains('@'));
        assert!(!debug.rotatable_by_detection);
    }

    #[test]
    fn describe_names_a_missing_key() {
        let f = Fixture::new();
        let err = describe(&f.ctx(), ".env", "GHOST").unwrap_err();
        assert_eq!(err.code, crate::errors::ErrorCode::NotFound);
        assert_eq!(err.data.unwrap()["reason"], "no_such_key");
    }

    #[test]
    fn generate_mints_writes_and_journals_without_disclosing() {
        let f = Fixture::new();
        let out = generate(
            &f.ctx(),
            &GenerateParams {
                path: ".env",
                key: "NEW_TOKEN",
                version: &f.version(".env"),
                shape: "hex(64)",
                comment: Some("minted by test"),
                intent: Some("wire the new service"),
            },
        )
        .unwrap();
        assert!(out.txn_id.is_some());
        assert_eq!(out.shape, "hex(64)");
        // the value landed on disk, 64 hex chars, and is NOT in the response
        let disk = f.disk(".env");
        let value = disk
            .lines()
            .find_map(|l| l.strip_prefix("NEW_TOKEN="))
            .expect("key written")
            .to_owned();
        assert_eq!(value.len(), 64);
        assert!(value.chars().all(|c| c.is_ascii_hexdigit()));
        let json = serde_json::to_string(&out).unwrap();
        assert!(!json.contains(&value), "generate leaked the value: {json}");
        assert!(out.placeholder.contains("NEW_TOKEN@"));
        assert!(disk.contains("# minted by test"));
        // the diff is redacted proof
        assert!(!out.diff.as_deref().unwrap_or("").contains(&value));
        // audit: one generate event, undisclosed, tied to the txn
        let events = f.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "generate");
        assert!(!events[0].disclosed);
        assert_eq!(events[0].txn_id, out.txn_id);
        assert_eq!(
            events[0].new_digest.as_deref(),
            Some(secrets::digest_of(&value).as_str())
        );
    }

    #[test]
    fn generate_refuses_an_existing_key() {
        let f = Fixture::new();
        let err = generate(
            &f.ctx(),
            &GenerateParams {
                path: ".env",
                key: "KLAMS_TOKEN",
                version: &f.version(".env"),
                shape: "hex(64)",
                comment: None,
                intent: None,
            },
        )
        .unwrap_err();
        assert!(err.message.contains("rotate"), "{}", err.message);
        assert_eq!(f.disk(".env").matches(TOKEN).count(), 1, "nothing changed");
        assert!(f.events().is_empty(), "no event for a refused generate");
    }

    #[test]
    fn generate_uses_named_shapes_from_config() {
        let mut f = Fixture::new();
        f.secrets.shapes.insert(
            "klams".into(),
            shapes::parse_spec("prefixed(klams-,hex(64))").unwrap(),
        );
        let out = generate(
            &f.ctx(),
            &GenerateParams {
                path: ".env",
                key: "K2",
                version: &f.version(".env"),
                shape: "klams",
                comment: None,
                intent: None,
            },
        )
        .unwrap();
        assert_eq!(out.shape, "prefixed(klams-,hex(64))");
        assert!(f.disk(".env").contains("K2=klams-"));

        // an unknown name still gets the grammar error, plus the names
        let err = generate(
            &f.ctx(),
            &GenerateParams {
                path: ".env",
                key: "K3",
                version: &f.version(".env"),
                shape: "nope",
                comment: None,
                intent: None,
            },
        )
        .unwrap_err();
        assert!(err.message.contains("klams"), "{}", err.message);
    }

    #[test]
    fn rotate_re_mints_like_for_like_and_journals_both_digests() {
        let f = Fixture::new();
        let (out, _value) = rotate_local(
            &f.ctx(),
            &RotateParams {
                path: ".env",
                key: "KLAMS_TOKEN",
                version: &f.version(".env"),
                shape: None, // detected: hex(32)
                also: &[],
                intent: None,
            },
        )
        .unwrap();
        assert_eq!(out.shape, "hex(32)");
        let disk = f.disk(".env");
        assert!(!disk.contains(TOKEN), "old value gone");
        let new_value = disk
            .lines()
            .find_map(|l| l.strip_prefix("KLAMS_TOKEN="))
            .unwrap();
        assert_eq!(new_value.len(), 32);
        assert_eq!(
            out.old_digest.as_deref(),
            Some(secrets::digest_of(TOKEN).as_str())
        );
        assert_eq!(
            out.new_digest.as_deref(),
            Some(secrets::digest_of(new_value).as_str())
        );
        let events = f.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "rotate");
        assert!(!events[0].disclosed);
    }

    #[test]
    fn rotate_refuses_an_undetectable_shape_without_an_explicit_one() {
        let f = Fixture::new();
        std::fs::write(
            f.roots[0].path.join(".env"),
            "PASSWORD=correct horse battery staple\n",
        )
        .unwrap();
        let err = rotate_local(
            &f.ctx(),
            &RotateParams {
                path: ".env",
                key: "PASSWORD",
                version: &f.version(".env"),
                shape: None,
                also: &[],
                intent: None,
            },
        )
        .unwrap_err();
        assert!(err.message.contains("shape"), "{}", err.message);
        // …and an explicit shape unblocks it
        let (out, _) = rotate_local(
            &f.ctx(),
            &RotateParams {
                path: ".env",
                key: "PASSWORD",
                version: &f.version(".env"),
                shape: Some("base64url(32)"),
                also: &[],
                intent: None,
            },
        )
        .unwrap();
        assert_eq!(out.shape, "base64url(32)");
    }

    #[test]
    fn rotate_writes_local_also_targets_in_one_transaction() {
        let f = Fixture::new();
        std::fs::write(
            f.roots[0].path.join("svc.env"),
            format!("CLIENT_TOKEN={TOKEN}\n"),
        )
        .unwrap();
        let (out, _) = rotate_local(
            &f.ctx(),
            &RotateParams {
                path: ".env",
                key: "KLAMS_TOKEN",
                version: &f.version(".env"),
                shape: None,
                also: &[AlsoTarget {
                    root: None,
                    path: "svc.env".into(),
                    key: Some("CLIENT_TOKEN".into()),
                    version: f.version("svc.env"),
                }],
                intent: None,
            },
        )
        .unwrap();
        // one txn, both files, same fresh value in both places
        assert_eq!(out.targets.len(), 2);
        let a = f.disk(".env");
        let b = f.disk("svc.env");
        let va = a
            .lines()
            .find_map(|l| l.strip_prefix("KLAMS_TOKEN="))
            .unwrap();
        let vb = b
            .lines()
            .find_map(|l| l.strip_prefix("CLIENT_TOKEN="))
            .unwrap();
        assert_eq!(va, vb);
        assert_ne!(va, TOKEN);
        // two audit rows, one per location, sharing the txn id
        let events = f.events();
        assert_eq!(events.len(), 2);
        assert!(events.iter().all(|e| e.txn_id == out.txn_id));

        // a stale also-version applies nothing anywhere
        let err = rotate_local(
            &f.ctx(),
            &RotateParams {
                path: ".env",
                key: "KLAMS_TOKEN",
                version: &f.version(".env"),
                shape: None,
                also: &[AlsoTarget {
                    root: None,
                    path: "svc.env".into(),
                    key: Some("CLIENT_TOKEN".into()),
                    version: "0000000000000000".into(),
                }],
                intent: None,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, crate::errors::ErrorCode::VersionConflict);
        assert_eq!(f.disk(".env"), a, "primary untouched after also-conflict");
    }

    #[test]
    fn occurrences_finds_digest_equality_across_files_and_roots() {
        let mut f = Fixture::new();
        std::fs::write(
            f.roots[0].path.join("svc.env"),
            format!("CLIENT_TOKEN={TOKEN}\nOTHER=abc\n"),
        )
        .unwrap();
        let second = tempfile::tempdir().unwrap();
        std::fs::write(second.path().join("app.env"), format!("COPY={TOKEN}\n")).unwrap();
        f.roots.push(ResolvedRoot::with_default_classify(
            "t:second",
            second.path().canonicalize().unwrap(),
        ));

        let out = occurrences(&f.ctx(), ".env", "KLAMS_TOKEN").unwrap();
        assert_eq!(out.digest, secrets::digest_of(TOKEN));
        assert_eq!(out.occurrences.len(), 3, "{:?}", out.occurrences);
        assert_eq!(
            out.occurrences.iter().filter(|o| o.is_source).count(),
            1,
            "{:?}",
            out.occurrences
        );
        assert!(
            out.occurrences
                .iter()
                .any(|o| o.root == "t:second" && o.key.as_deref() == Some("COPY"))
        );
        assert!(out.files_searched >= 3);
        assert!(out.coverage_note.contains("#1053"));
    }

    #[test]
    fn occurrences_refuses_below_floor_values() {
        let f = Fixture::new();
        let err = occurrences(&f.ctx(), ".env", "DEBUG").unwrap_err();
        assert!(err.message.contains("entropy floor"), "{}", err.message);
    }

    #[test]
    fn reveal_disclosed_and_journaled_with_required_intent() {
        let f = Fixture::new();
        let err = reveal(
            &f.ctx(),
            &RevealParams {
                path: ".env",
                key: "KLAMS_TOKEN",
                intent: "  ",
                expected_digest: None,
                transport_destination: None,
            },
        )
        .unwrap_err();
        assert!(err.message.contains("intent"), "{}", err.message);
        assert!(f.events().is_empty(), "a refused reveal discloses nothing");

        let out = reveal(
            &f.ctx(),
            &RevealParams {
                path: ".env",
                key: "KLAMS_TOKEN",
                intent: "paste into cleo's MCP client config, which kaed cannot write",
                expected_digest: None,
                transport_destination: None,
            },
        )
        .unwrap();
        assert_eq!(out.value, TOKEN);
        assert!(out.disclosed);
        let events = f.events();
        assert_eq!(events.len(), 1);
        assert_eq!(events[0].action, "reveal");
        assert!(events[0].disclosed);
        assert!(events[0].intent.as_deref().unwrap().contains("cleo"));
    }

    #[test]
    fn reveal_verifies_an_expected_digest_loudly() {
        let f = Fixture::new();
        let err = reveal(
            &f.ctx(),
            &RevealParams {
                path: ".env",
                key: "KLAMS_TOKEN",
                intent: "verify",
                expected_digest: Some("0000000000000000"),
                transport_destination: None,
            },
        )
        .unwrap_err();
        assert_eq!(err.data.as_ref().unwrap()["reason"], "digest_mismatch");
        assert!(f.events().is_empty(), "mismatch discloses nothing");
    }

    #[test]
    fn reveal_respects_the_config_kill_switch() {
        let mut f = Fixture::new();
        f.secrets.allow_reveal = false;
        let err = reveal(
            &f.ctx(),
            &RevealParams {
                path: ".env",
                key: "KLAMS_TOKEN",
                intent: "try anyway",
                expected_digest: None,
                transport_destination: None,
            },
        )
        .unwrap_err();
        assert_eq!(err.code, crate::errors::ErrorCode::Denied);
        assert_eq!(err.data.unwrap()["reason"], "reveal_disabled");
    }

    #[test]
    fn reveal_refuses_unclassified_files() {
        let f = Fixture::new();
        std::fs::write(f.roots[0].path.join("plain.conf"), "MODE=fast\n").unwrap();
        let err = reveal(
            &f.ctx(),
            &RevealParams {
                path: "plain.conf",
                key: "MODE",
                intent: "why not",
                expected_digest: None,
                transport_destination: None,
            },
        )
        .unwrap_err();
        assert!(err.message.contains("read it directly"), "{}", err.message);
    }

    #[test]
    fn transport_reveal_journals_the_claimed_destination() {
        let f = Fixture::new();
        let out = reveal(
            &f.ctx(),
            &RevealParams {
                path: ".env",
                key: "KLAMS_TOKEN",
                intent: "cross-host value_from",
                expected_digest: Some(&secrets::digest_of(TOKEN)),
                transport_destination: Some("kubs0:src/svc/.env"),
            },
        )
        .unwrap();
        assert_eq!(out.value, TOKEN);
        let events = f.events();
        assert_eq!(events[0].action, "transport");
        assert!(events[0].disclosed);
        assert_eq!(events[0].destination.as_deref(), Some("kubs0:src/svc/.env"));
    }

    #[test]
    fn the_journal_kind_secret_serves_the_stream_with_coverage() {
        let f = Fixture::new();
        generate(
            &f.ctx(),
            &GenerateParams {
                path: ".env",
                key: "G1",
                version: &f.version(".env"),
                shape: "hex(64)",
                comment: None,
                intent: None,
            },
        )
        .unwrap();
        let out = crate::history::journal(
            &f.journal,
            &f.roots,
            "t",
            &crate::history::JournalQuery {
                root: None,
                path: None,
                author: None,
                since: None,
                kinds: vec![RecordKind::Secret],
                max: 10,
            },
        )
        .unwrap();
        assert_eq!(out.entries.len(), 1);
        match &out.entries[0] {
            crate::history::Entry::Secret {
                action, disclosed, ..
            } => {
                assert_eq!(action, "generate");
                assert!(!disclosed);
            }
            other => panic!("expected a secret entry, got {other:?}"),
        }
        assert!(out.coverage.secrets_from.is_some());
        // and the default (no kinds) includes the stream
        let all = crate::history::journal(
            &f.journal,
            &f.roots,
            "t",
            &crate::history::JournalQuery {
                root: None,
                path: None,
                author: None,
                since: None,
                kinds: vec![],
                max: 10,
            },
        )
        .unwrap();
        assert!(
            all.entries
                .iter()
                .any(|e| matches!(e, crate::history::Entry::Secret { .. }))
        );
    }
}
