//! In-repo policy: `.kaedignore`, the in-file marker, and classification.
//!
//! Three layers sit above the server deny list, in decreasing absoluteness
//! (server config > `.kaedignore` > in-file marker > classification), and
//! each may only *add* restriction — a policy file can never relax what a
//! stronger layer denied. That invariant is why `.kaedignore` negation is
//! evaluated per-file (D-3): `!pattern` can un-deny something the same
//! file denied, never something another file or the server did. Otherwise
//! the first thing a compromised agent writes is `!~/.ssh`.
//!
//! Classification is the deny-vs-classify distinction from the secrets
//! brainstorm: heuristics classify (→ redact, or refuse with a precise
//! reason), only explicit config denies. It is lexical like the deny list
//! — it answers identically for paths that exist and paths that don't.

use ignore::gitignore::{Gitignore, GitignoreBuilder};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub const KAEDIGNORE: &str = ".kaedignore";

/// Files the heuristics classify as secret-bearing. These are deliberately
/// *not* in `deny::DEFAULT_DENY`: a heuristic hard-deny is a confusing
/// dead end, classification degrades gracefully — dotenv-shaped files get
/// a redacted surface, the rest a structured refusal (D-1, D-2).
pub const DEFAULT_CLASSIFY: &[&str] = &[
    "**/.env",
    "**/.env.*",
    "**/*.env",
    "**/*.pem",
    "**/*.p12",
    "**/id_rsa",
    "**/id_ecdsa",
    "**/id_ed25519",
    "**/credentials*",
    "**/*.kdbx",
];

// ------------------------------------------------------------ classifier

#[derive(Debug)]
pub struct Classifier {
    set: globset::GlobSet,
    patterns: Vec<String>,
}

impl Classifier {
    pub fn new(globs: &[String]) -> Result<Classifier, globset::Error> {
        let mut b = globset::GlobSetBuilder::new();
        let mut patterns = Vec::new();
        for g in globs {
            b.add(globset::Glob::new(g)?);
            patterns.push(g.clone());
        }
        Ok(Classifier {
            set: b.build()?,
            patterns,
        })
    }

    pub fn empty() -> Classifier {
        Classifier::new(&[]).expect("empty glob set builds")
    }

    /// The rule classifying `abs`, or `None`. Ancestor-walking like
    /// `DenyList::denied_by`, so a rule naming a directory covers its tree.
    pub fn classified_by(&self, abs: &Path) -> Option<String> {
        let mut cur = Some(abs);
        while let Some(p) = cur {
            let hits = self.set.matches(p);
            if let Some(&i) = hits.first() {
                return Some(self.patterns[i].clone());
            }
            cur = p.parent();
        }
        None
    }

    pub fn describe(&self) -> Vec<String> {
        self.patterns.clone()
    }
}

// ------------------------------------------------------- .kaedignore stack

#[derive(Debug)]
pub struct KaedignoreMatch {
    /// The `.kaedignore` file whose rule denied the path — named in the
    /// refusal so the agent can go read the policy.
    pub file: PathBuf,
    pub pattern: String,
}

/// Per-call matcher cache for the walkers: each `.kaedignore` is read and
/// parsed at most once per `list`/`search` call. No cross-call state — the
/// filesystem stays the source of truth.
pub struct KaedignoreCache {
    root: PathBuf,
    matchers: Mutex<HashMap<PathBuf, Option<Arc<Gitignore>>>>,
}

impl KaedignoreCache {
    pub fn new(root: PathBuf) -> Self {
        Self {
            root,
            matchers: Mutex::new(HashMap::new()),
        }
    }

    /// The `.kaedignore` rule denying `abs`, or `None`. Every
    /// `.kaedignore` from the root down to the path's directory is
    /// consulted **independently**: a file's own `!` negations apply to
    /// its own rules and to nothing else (D-3).
    ///
    /// `.kaedignore` files themselves are exempt — a refusal names the
    /// file that caused it, so the agent must always be able to read it.
    pub fn denied(&self, abs: &Path) -> Option<KaedignoreMatch> {
        if abs.file_name().is_some_and(|n| n == KAEDIGNORE) {
            return None;
        }
        let rel = abs.strip_prefix(&self.root).ok()?;
        if rel.as_os_str().is_empty() {
            return None; // the root itself is never kaedignore-denied
        }
        let is_dir = abs.is_dir();
        let mut dir = self.root.clone();
        // the chain: root itself, then every ancestor directory of `abs`
        let mut dirs = vec![dir.clone()];
        for comp in rel.parent().map(Path::components).into_iter().flatten() {
            dir.push(comp);
            dirs.push(dir.clone());
        }
        for d in dirs {
            let Some(matcher) = self.matcher(&d) else {
                continue;
            };
            if let ignore::Match::Ignore(glob) = matcher.matched_path_or_any_parents(abs, is_dir) {
                return Some(KaedignoreMatch {
                    file: d.join(KAEDIGNORE),
                    pattern: glob.original().to_owned(),
                });
            }
        }
        None
    }

    fn matcher(&self, dir: &Path) -> Option<Arc<Gitignore>> {
        let mut cache = self.matchers.lock().expect("kaedignore cache lock");
        if let Some(m) = cache.get(dir) {
            return m.clone();
        }
        let path = dir.join(KAEDIGNORE);
        let built = if path.is_file() {
            let mut b = GitignoreBuilder::new(dir);
            if let Some(e) = b.add(&path) {
                tracing::warn!(file = %path.display(), error = %e, "unparseable .kaedignore line(s) skipped");
            }
            match b.build() {
                Ok(g) => Some(Arc::new(g)),
                Err(e) => {
                    tracing::warn!(file = %path.display(), error = %e, "ignoring unusable .kaedignore");
                    None
                }
            }
        } else {
            None
        };
        cache.insert(dir.to_path_buf(), built.clone());
        built
    }
}

/// One-shot check for addressed paths (the resolvers). Builds a throwaway
/// cache; the walkers hold a `KaedignoreCache` across their whole walk.
pub fn kaedignore_denied(root_path: &Path, abs: &Path) -> Option<KaedignoreMatch> {
    KaedignoreCache::new(root_path.to_path_buf()).denied(abs)
}

pub fn is_kaedignore_path(rel: &str) -> bool {
    Path::new(rel).file_name().is_some_and(|n| n == KAEDIGNORE)
}

// ---------------------------------------------------------- in-file marker

/// Lines of a file inspected for the marker. In-band signalling gets a
/// short leash: content controls policy here (the same shape as prompt
/// injection), and the line limit plus the strict token below are the
/// mitigations the WI requires writing down (D-4).
pub const MARKER_LINES: usize = 5;

/// True when one of the first `MARKER_LINES` lines is the bare token
/// `kaedignore` on a comment line — `# kaedignore`, `// kaedignore`,
/// `<!-- kaedignore -->`. Prose *mentioning* the token does not trigger.
pub fn has_ignore_marker(content: &str) -> bool {
    content.lines().take(MARKER_LINES).any(is_marker_line)
}

fn is_marker_line(line: &str) -> bool {
    let mut s = line.trim();
    for leader in ["#", "//", ";", "--", "/*", "<!--", "*"] {
        if let Some(rest) = s.strip_prefix(leader) {
            s = rest;
            break;
        }
    }
    let mut s = s.trim();
    for closer in ["-->", "*/"] {
        if let Some(rest) = s.strip_suffix(closer) {
            s = rest;
            break;
        }
    }
    s.trim() == "kaedignore"
}

// --------------------------------------------------------- gitignore check

/// A warning when a classified file inside a git repo is **not**
/// git-ignored — one commit from publishing a live secret. Best-effort via
/// `git check-ignore` (the same stance as `git_head` in the journal): no
/// git, no repo, or a git error means no warning, never a failure.
pub fn gitignore_warning(abs: &Path, rel: &str) -> Option<String> {
    let dir = abs.parent()?;
    find_git_root(dir)?;
    let status = std::process::Command::new("git")
        .arg("-C")
        .arg(dir)
        .args(["check-ignore", "-q", "--"])
        .arg(abs)
        .status()
        .ok()?;
    match status.code() {
        Some(1) => Some(format!(
            "{rel} is classified as secret-bearing but is NOT gitignored — a commit \
             would publish it; add it to .gitignore"
        )),
        _ => None, // 0 = ignored; anything else = best-effort gives up
    }
}

fn find_git_root(start: &Path) -> Option<PathBuf> {
    let mut cur = Some(start);
    while let Some(dir) = cur {
        if dir.join(".git").exists() {
            return Some(dir.to_path_buf());
        }
        cur = dir.parent();
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    // ------------------------------------------------------- .kaedignore

    #[test]
    fn kaedignore_denies_by_gitignore_shaped_rules() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        write(&root, ".kaedignore", "notes-private/\n*.secret\n");
        write(&root, "notes-private/journal.md", "x");
        write(&root, "plans.secret", "x");
        write(&root, "plans.md", "x");

        let hit = kaedignore_denied(&root, &root.join("plans.secret")).unwrap();
        assert_eq!(hit.pattern, "*.secret");
        assert_eq!(hit.file, root.join(".kaedignore"));
        // a rule on a directory covers what's beneath it
        assert!(kaedignore_denied(&root, &root.join("notes-private/journal.md")).is_some());
        assert!(kaedignore_denied(&root, &root.join("plans.md")).is_none());
    }

    #[test]
    fn negation_undenies_within_one_file_only() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        // the root file denies *.secret but takes one back — legal
        write(&root, ".kaedignore", "*.secret\n!allowed.secret\n");
        // the deeper file tries to relax the ROOT's rule — must not work
        write(&root, "sub/.kaedignore", "!*.secret\n");
        write(&root, "allowed.secret", "x");
        write(&root, "denied.secret", "x");
        write(&root, "sub/denied.secret", "x");

        assert!(
            kaedignore_denied(&root, &root.join("allowed.secret")).is_none(),
            "same-file negation is the gitignore idiom and stays legal"
        );
        assert!(kaedignore_denied(&root, &root.join("denied.secret")).is_some());
        assert!(
            kaedignore_denied(&root, &root.join("sub/denied.secret")).is_some(),
            "a deeper .kaedignore must never relax a shallower one's rule"
        );
    }

    #[test]
    fn kaedignore_files_are_always_readable() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        // even a self-denying policy file stays readable: refusals name it,
        // so the agent must be able to go look at it
        write(&root, ".kaedignore", "*\n");
        assert!(kaedignore_denied(&root, &root.join(".kaedignore")).is_none());
        assert!(kaedignore_denied(&root, &root.join("sub/.kaedignore")).is_none());
        assert!(kaedignore_denied(&root, &root.join("anything.txt")).is_some());
    }

    #[test]
    fn a_missing_kaedignore_denies_nothing() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        write(&root, "f.txt", "x");
        assert!(kaedignore_denied(&root, &root.join("f.txt")).is_none());
    }

    // ------------------------------------------------------------ marker

    #[test]
    fn marker_is_a_strict_token_in_the_first_five_lines() {
        for hit in [
            "# kaedignore\nKEY=1\n",
            "// kaedignore\ncode();\n",
            "<!-- kaedignore -->\n<html>\n",
            "-- kaedignore\nSELECT 1;\n",
            "a\nb\nc\nd\n# kaedignore\n", // line 5 still counts
        ] {
            assert!(has_ignore_marker(hit), "{hit:?}");
        }
        for miss in [
            // line 6 is past the window — a match there would make every
            // long file mentioning the feature unreadable
            "a\nb\nc\nd\ne\n# kaedignore\n",
            // prose about the marker is not the marker
            "# add `# kaedignore` to a file to hide it from kaed\n",
            "# kaedignored\n",
            "kaedignore=true\n",
            "plain text\n",
        ] {
            assert!(!has_ignore_marker(miss), "{miss:?}");
        }
    }

    // -------------------------------------------------------- classifier

    /// The 004 lesson (`**/secrets/**` never matched the directory it
    /// named): every default rule gets an assertion that it matches what a
    /// human reading it expects.
    #[test]
    fn default_classify_rules_match_what_they_read_like_they_match() {
        let c = Classifier::new(
            &DEFAULT_CLASSIFY
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        for classified in [
            "/home/ken/src/app/.env",
            "/home/ken/src/app/.env.production",
            "/datastore/korg/korg.env", // the kubsdb file that motivates #929
            "/home/ken/certs/server.pem",
            "/home/ken/certs/bundle.p12",
            "/home/ken/stray/id_ed25519",
            "/home/ken/gcp/credentials.json",
            "/home/ken/vault/passwords.kdbx",
        ] {
            assert!(
                c.classified_by(Path::new(classified)).is_some(),
                "{classified} should classify"
            );
        }
        for plain in [
            "/home/ken/src/app/src/main.rs",
            "/home/ken/src/app/environment.md",
            "/home/ken/src/app/.envrc", // direnv shell code, not dotenv
            "/home/ken/notes/env-setup.md",
        ] {
            assert!(
                c.classified_by(Path::new(plain)).is_none(),
                "{plain} should not classify"
            );
        }
    }

    /// Sprint 014 / korg #1093, the sibling of 013's
    /// `the_kubsdb_broad_access_shape_holds`: these are the exact
    /// `[security] classify` globs kubsdb's config carries, so matcher
    /// drift fails here rather than on the host.
    ///
    /// The precision is the decision. Six files under `/datastore` hold
    /// credentials inline, and none is `.env`-shaped — today they are
    /// protected by unix mode alone, which is luck rather than policy, and
    /// korg #1085 opened by proposing to change exactly those modes. A
    /// tempting `**/docker-compose.yml` would have classified the whole
    /// managed set too, and those are world-readable, hold no secrets, and
    /// are useful to read: making them opaque would be a regression 013's
    /// live test would have caught.
    #[test]
    fn the_kubsdb_classify_shape_holds() {
        const KUBSDB_CLASSIFY: &[&str] = &[
            "/datastore/postgresql/docker-compose.yml",
            "/datastore/mongodb/docker-compose.yml",
            "/datastore/redis/docker-compose.yml",
            "/datastore/unpoller/up.conf",
            "/datastore/grafana/provisioning/datasources/datasources.yml",
        ];
        let c = Classifier::new(
            &KUBSDB_CLASSIFY
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        for secret in KUBSDB_CLASSIFY {
            assert!(
                c.classified_by(Path::new(secret)).is_some(),
                "{secret} carries a credential inline and must classify"
            );
        }
        // The k-homelab-managed set: rendered artifacts, world-readable, no
        // secrets. Reading them is genuinely useful — only *writing* them is
        // futile, and that is #1091's job to explain, not this one's.
        for managed in [
            "/datastore/grafana/docker-compose.yml",
            "/datastore/prometheus/docker-compose.yml",
            "/datastore/prometheus/prometheus.yml",
            "/datastore/package-store/docker-compose.yml",
            "/datastore/package-store/nginx.conf",
            "/datastore/registry/docker-compose.yml",
            "/datastore/grafana/provisioning/dashboards/dashboards.yml",
            "/datastore/korg/docker-compose.yml",
        ] {
            assert!(
                c.classified_by(Path::new(managed)).is_none(),
                "{managed} is a rendered artifact, not a secret — it must stay readable"
            );
        }
        // grafana's provisioning tree classifies at exactly one file, not
        // wholesale: `datasources.yml` is 0640 root:root by k-homelab
        // sprint 016's deliberate choice, and its neighbours are not.
        assert!(
            c.classified_by(Path::new("/datastore/grafana/provisioning"))
                .is_none()
        );
    }

    #[test]
    fn classification_is_lexical_and_covers_subtrees() {
        let c = Classifier::new(&["**/vault-files".to_string()]).unwrap();
        // never touches the filesystem: nonexistent paths answer the same
        assert!(
            c.classified_by(Path::new("/nowhere/vault-files/deep/x"))
                .is_some()
        );
        assert!(c.classified_by(Path::new("/nowhere/other/x")).is_none());
        assert!(
            Classifier::empty()
                .classified_by(Path::new("/a/.env"))
                .is_none()
        );
    }

    // --------------------------------------------------- gitignore check

    #[test]
    fn gitignore_warning_fires_only_in_a_repo_that_does_not_ignore() {
        let dir = tempfile::tempdir().unwrap();
        let root = dir.path().canonicalize().unwrap();
        write(&root, ".env", "TOKEN=x\n");
        // no repo → best-effort silence
        assert!(gitignore_warning(&root.join(".env"), ".env").is_none());

        let run = |args: &[&str]| {
            let st = std::process::Command::new("git")
                .arg("-C")
                .arg(&root)
                .args(args)
                .output()
                .unwrap();
            assert!(st.status.success(), "git {args:?}: {st:?}");
        };
        run(&["init", "-q"]);
        let w = gitignore_warning(&root.join(".env"), ".env")
            .expect("unignored classified file in a repo warns");
        assert!(w.contains("NOT gitignored"), "{w}");

        write(&root, ".gitignore", ".env\n");
        assert!(
            gitignore_warning(&root.join(".env"), ".env").is_none(),
            "ignored file is the safe state — no warning"
        );
    }
}
