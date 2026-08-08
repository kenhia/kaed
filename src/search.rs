//! Server-side search (ripgrep engine). Each match carries its file's
//! `version`, so search → edit works with no read in between: a stale hit
//! becomes `version_conflict`, never a wrong edit.
//!
//! Files are read once and searched as a slice — the version is hashed
//! from exactly the bytes that were searched. Binary and over-limit files
//! are skipped silently, like ripgrep. File order is sorted, so results
//! and truncation are deterministic.

use crate::config::{Limits, ResolvedRoot};
use crate::dotenv;
use crate::errors::{KaedError, Result};
use crate::fsops;
use crate::policy;
use grep_matcher::Matcher;
use grep_regex::{RegexMatcher, RegexMatcherBuilder};
use grep_searcher::{Searcher, SearcherBuilder, Sink, SinkContext, SinkContextKind, SinkMatch};
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchMatch {
    /// Root-relative path.
    pub path: String,
    /// Version of the file the match was found in — a valid edit base.
    pub version: String,
    /// 1-based line of the match.
    pub line: usize,
    /// 1-based byte column of the match start within the line.
    pub col: usize,
    /// The matched line, without its trailing newline.
    pub text: String,
    pub before: Vec<String>,
    pub after: Vec<String>,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct SearchResult {
    pub matches: Vec<SearchMatch>,
    pub truncated: bool,
    /// Files actually opened and searched, after `path`, `glob`, the deny
    /// list, and the binary/size skips. **Always present, including zero**:
    /// `0 matches in 41 files` and `0 matches in 0 files` were byte-identical
    /// answers until korg #1066, and the second one is a scoping mistake
    /// rather than evidence of anything.
    pub files_searched: usize,
    /// Why an empty result is empty, when it is more likely to be the
    /// caller's `glob`/`path` than the truth about the tree.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub reason: Option<fsops::EmptyReason>,
    /// Files the deny list (or a `.kaedignore`, or an in-file marker) kept
    /// out of the search entirely. Omitted when zero — "no matches" and
    /// "no matches in what I was allowed to read" are different answers.
    #[serde(skip_serializing_if = "is_zero")]
    pub denied_hidden: usize,
    /// Classified files with no redacted surface, skipped whole. Sibling
    /// of `denied_hidden`, same R7 honesty rule. Classified *dotenv* files
    /// are not counted here: they are searched, over their redacted
    /// rendering (D-8).
    #[serde(skip_serializing_if = "is_zero")]
    pub classified_hidden: usize,
    /// Entries the OS refused: directories that could not be descended and
    /// files that could not be opened, because of unix permissions rather
    /// than any kaed policy (014, korg #1088). Third sibling, same rule —
    /// and deliberately NOT folded into `denied_hidden`, because "policy
    /// says no" and "the OS said no" have different remedies.
    #[serde(skip_serializing_if = "is_zero")]
    pub unreadable_hidden: usize,
}

fn is_zero(n: &usize) -> bool {
    *n == 0
}

pub struct SearchParams<'a> {
    pub pattern: &'a str,
    /// `false` searches the pattern as a literal string.
    pub regex: bool,
    pub glob: Option<&'a str>,
    /// Subtree (or single file) to search; empty = whole root.
    pub path: &'a str,
    pub context: usize,
    pub max_results: usize,
}

pub fn search(root: &ResolvedRoot, p: &SearchParams, limits: &Limits) -> Result<SearchResult> {
    let matcher = RegexMatcherBuilder::new()
        .fixed_strings(!p.regex)
        .build(p.pattern)
        .map_err(|e| KaedError::invalid_input(format!("bad pattern {:?}: {e}", p.pattern)))?;
    let glob = match p.glob {
        Some(g) => Some(
            globset::GlobBuilder::new(g)
                .literal_separator(false)
                .build()
                .map_err(|e| KaedError::invalid_input(format!("bad glob {g:?}: {e}")))?
                .compile_matcher(),
        ),
        None => None,
    };

    let base = fsops::resolve_existing(root, p.path)?;
    // As in `list`, this walk bypasses the resolver — and here a missed
    // deny check would hand back the *contents* of a denied file, not just
    // its name.
    let denied_hidden = std::sync::Arc::new(std::sync::atomic::AtomicUsize::new(0));
    let mut unreadable_hidden = 0usize;
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    // Addressed, not enumerated (014 D-6): whatever `path` NAMED gets a
    // reason if the OS refuses it. Only what the walk *finds* is skipped
    // and counted — a zero for the file you asked about, with a count of
    // one beside it, is the #1091 complaint wearing a different hat.
    let addressed_file = base.is_file();
    if !addressed_file && !crate::perm::dir_is_readable(&base) {
        return Err(crate::perm::not_readable(root, p.path, &base));
    }
    if addressed_file {
        files.push(base);
    } else {
        let walker = {
            let deny = root.deny.clone();
            let kaedignore = policy::KaedignoreCache::new(root.path.clone());
            let counter = denied_hidden.clone();
            ignore::WalkBuilder::new(&base)
                .hidden(false)
                .filter_entry(move |e| {
                    if e.file_name() == ".git" {
                        return false;
                    }
                    if deny.is_denied(e.path()) || kaedignore.denied(e.path()).is_some() {
                        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        return false;
                    }
                    true
                })
                .build()
        };
        for entry in walker {
            let entry = match entry {
                Ok(e) => e,
                // One `drwx------` directory used to kill the whole call
                // (#1088). The OS is a filter like any other: skip it,
                // count it, keep walking. Non-permission walk errors still
                // fail loudly — they are not a coverage question.
                Err(e) if walk_error_is_permission(&e) => {
                    unreadable_hidden += 1;
                    continue;
                }
                Err(e) => return Err(KaedError::internal(e.to_string())),
            };
            if entry.file_type().is_some_and(|t| t.is_file()) {
                files.push(entry.into_path());
            }
        }
        files.sort();
    }

    let mut searcher = SearcherBuilder::new()
        .line_number(true)
        .before_context(p.context)
        .after_context(p.context)
        .build();

    let mut matches = Vec::new();
    let mut truncated = false;
    let mut classified_hidden = 0usize;
    let mut tally = fsops::ScopeTally {
        candidates: files.len(),
        ..Default::default()
    };
    'files: for abs in &files {
        let rel = abs
            .strip_prefix(&root.path)
            .unwrap_or(abs)
            .to_string_lossy()
            .into_owned();
        if let Some(g) = &glob
            && !g.is_match(&rel)
        {
            continue;
        }
        tally.kept += 1;
        let meta = match std::fs::metadata(abs) {
            Ok(m) => m,
            Err(e) => {
                if crate::perm::is_permission_denied(&e) {
                    unreadable_hidden += 1;
                }
                continue;
            }
        };
        if meta.len() > limits.max_file_bytes {
            continue;
        }
        // A reachable-but-unopenable file (root-owned 0600 config) was a
        // silent skip before #1088 — the same R7 dishonesty one level down
        // from the walker.
        let bytes = match std::fs::read(abs) {
            Ok(b) => b,
            Err(e) => {
                if crate::perm::is_permission_denied(&e) {
                    if addressed_file {
                        return Err(crate::perm::not_readable(root, p.path, abs));
                    }
                    unreadable_hidden += 1;
                }
                continue;
            }
        };
        let text = if fsops::looks_binary(&bytes) {
            None
        } else {
            std::str::from_utf8(&bytes).ok()
        };
        // in-file marker: the file opted out — same deny layer, same count
        if text.is_some_and(policy::has_ignore_marker) {
            denied_hidden.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            continue;
        }
        // Classified files are never searched raw. Dotenv-shaped ones are
        // searched over the same redacted rendering `read` serves — so a
        // pattern probing for a secret's *value* matches nothing, by
        // construction rather than by filtering (D-8). The rest are
        // skipped whole and counted.
        let searched: std::borrow::Cow<'_, [u8]>;
        let mut version = None;
        if root.classify.classified_by(abs).is_some() {
            let Some(file) = text.and_then(dotenv::parse) else {
                classified_hidden += 1;
                continue;
            };
            // the version each hit carries stays the RAW file's — it is a
            // content address of the bytes on disk, and the edit base (R1)
            version = Some(fsops::version_of(&bytes));
            searched = std::borrow::Cow::Owned(file.redact().into_bytes());
        } else {
            if text.is_none() {
                continue; // binary or non-UTF-8, skipped like ripgrep
            }
            searched = std::borrow::Cow::Borrowed(&bytes);
        }
        tally.opened += 1;
        let mut sink = CollectSink {
            matcher: &matcher,
            rel: &rel,
            version,
            bytes: &searched,
            matches: &mut matches,
            before_buf: Vec::new(),
            budget: p.max_results,
            hit_budget: false,
        };
        searcher
            .search_slice(&matcher, &searched, &mut sink)
            .map_err(|e| KaedError::internal(format!("{rel}: {e}")))?;
        if sink.hit_budget {
            truncated = true;
            break 'files;
        }
    }

    Ok(SearchResult {
        reason: matches
            .is_empty()
            .then(|| tally.explain(p.glob, p.path))
            .flatten(),
        matches,
        truncated,
        files_searched: tally.opened,
        denied_hidden: denied_hidden.load(std::sync::atomic::Ordering::Relaxed),
        classified_hidden,
        unreadable_hidden,
    })
}

/// Is this walk error the OS refusing on permissions? `ignore` wraps the
/// underlying `io::Error`, so the kind survives the wrapping.
fn walk_error_is_permission(e: &ignore::Error) -> bool {
    e.io_error().is_some_and(crate::perm::is_permission_denied)
}

struct CollectSink<'a> {
    matcher: &'a RegexMatcher,
    rel: &'a str,
    /// Hashed lazily on the first match in this file.
    version: Option<String>,
    bytes: &'a [u8],
    matches: &'a mut Vec<SearchMatch>,
    /// Before-context lines waiting for the match they precede.
    before_buf: Vec<String>,
    budget: usize,
    hit_budget: bool,
}

impl Sink for CollectSink<'_> {
    type Error = std::io::Error;

    fn matched(&mut self, _searcher: &Searcher, mat: &SinkMatch<'_>) -> std::io::Result<bool> {
        if self.matches.len() >= self.budget {
            self.hit_budget = true;
            return Ok(false);
        }
        let version = self
            .version
            .get_or_insert_with(|| fsops::version_of(self.bytes))
            .clone();
        let line_bytes = mat.bytes();
        let col = self
            .matcher
            .find(line_bytes)
            .ok()
            .flatten()
            .map(|m| m.start() + 1)
            .unwrap_or(1);
        self.matches.push(SearchMatch {
            path: self.rel.to_owned(),
            version,
            line: mat.line_number().unwrap_or(0) as usize,
            col,
            text: as_line(line_bytes),
            before: std::mem::take(&mut self.before_buf),
            after: Vec::new(),
        });
        Ok(true)
    }

    fn context(&mut self, _searcher: &Searcher, ctx: &SinkContext<'_>) -> std::io::Result<bool> {
        // Before-context precedes its match in the stream; After follows it.
        match ctx.kind() {
            SinkContextKind::After => {
                if let Some(last) = self.matches.last_mut()
                    && last.path == self.rel
                {
                    last.after.push(as_line(ctx.bytes()));
                }
            }
            _ => self.before_buf.push(as_line(ctx.bytes())),
        }
        Ok(true)
    }
}

fn as_line(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes)
        .trim_end_matches('\n')
        .to_owned()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorCode;
    use std::path::Path;

    fn setup() -> (tempfile::TempDir, ResolvedRoot) {
        let dir = tempfile::tempdir().unwrap();
        let root = ResolvedRoot::unrestricted("t", dir.path().canonicalize().unwrap());
        (dir, root)
    }

    fn write(dir: &Path, rel: &str, content: &str) {
        let p = dir.join(rel);
        std::fs::create_dir_all(p.parent().unwrap()).unwrap();
        std::fs::write(p, content).unwrap();
    }

    fn params<'a>(pattern: &'a str) -> SearchParams<'a> {
        SearchParams {
            pattern,
            regex: true,
            glob: None,
            path: "",
            context: 2,
            max_results: 50,
        }
    }

    /// The #908 case that a resolver-only deny check would have missed:
    /// `search` reads every file it walks and returns the matching lines,
    /// so an unfiltered walk hands back the *contents* of a denied file.
    #[test]
    fn denied_files_are_searched_neither_by_name_nor_content() {
        let (dir, _) = setup();
        write(dir.path(), "src/app.rs", "let token = env(\"TOKEN\");\n");
        write(dir.path(), ".config/kaed/env", "KAED_TOKEN_CLAUDE=sekrit\n");
        let root = ResolvedRoot {
            deny: std::sync::Arc::new(
                crate::deny::DenyList::new(Vec::new(), &["**/.config/kaed".to_string()]).unwrap(),
            ),
            ..ResolvedRoot::unrestricted("t", dir.path().canonicalize().unwrap())
        };

        let r = search(&root, &params("TOKEN"), &Limits::default()).unwrap();
        assert_eq!(r.matches.len(), 1);
        assert_eq!(r.matches[0].path, "src/app.rs");
        assert!(
            !r.matches.iter().any(|m| m.text.contains("sekrit")),
            "denied file content leaked into search results"
        );
        assert_eq!(r.denied_hidden, 1);
    }

    #[test]
    fn match_carries_version_line_col_and_context() {
        let (dir, root) = setup();
        write(dir.path(), "f.txt", "one\ntwo\nfn target() {\nfour\nfive\n");
        let r = search(&root, &params("target"), &Limits::default()).unwrap();
        assert_eq!(r.matches.len(), 1);
        let m = &r.matches[0];
        assert_eq!(m.path, "f.txt");
        assert_eq!(m.line, 3);
        assert_eq!(m.col, 4);
        assert_eq!(m.text, "fn target() {");
        assert_eq!(m.before, vec!["one", "two"]);
        assert_eq!(m.after, vec!["four", "five"]);
        assert_eq!(
            m.version,
            fsops::version_of(b"one\ntwo\nfn target() {\nfour\nfive\n")
        );
        assert!(!r.truncated);
    }

    #[test]
    fn literal_mode_escapes_regex_metachars() {
        let (dir, root) = setup();
        write(dir.path(), "f.txt", "abc\na.c\n");
        let rx = search(&root, &params("a.c"), &Limits::default()).unwrap();
        assert_eq!(rx.matches.len(), 2);
        let lit = search(
            &root,
            &SearchParams {
                regex: false,
                ..params("a.c")
            },
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(lit.matches.len(), 1);
        assert_eq!(lit.matches[0].line, 2);
    }

    #[test]
    fn glob_and_path_narrow_the_search() {
        let (dir, root) = setup();
        write(dir.path(), "a.rs", "needle\n");
        write(dir.path(), "b.txt", "needle\n");
        write(dir.path(), "sub/c.rs", "needle\n");
        let globbed = search(
            &root,
            &SearchParams {
                glob: Some("**/*.rs"),
                ..params("needle")
            },
            &Limits::default(),
        )
        .unwrap();
        let paths: Vec<_> = globbed.matches.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["a.rs", "sub/c.rs"]);

        let scoped = search(
            &root,
            &SearchParams {
                path: "sub",
                ..params("needle")
            },
            &Limits::default(),
        )
        .unwrap();
        let paths: Vec<_> = scoped.matches.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["sub/c.rs"]);

        let single = search(
            &root,
            &SearchParams {
                path: "b.txt",
                ..params("needle")
            },
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(single.matches.len(), 1);
    }

    #[test]
    fn max_results_truncates_deterministically() {
        let (dir, root) = setup();
        for i in 0..5 {
            write(dir.path(), &format!("f{i}.txt"), "hit\nhit\n");
        }
        let r = search(
            &root,
            &SearchParams {
                max_results: 3,
                ..params("hit")
            },
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.matches.len(), 3);
        assert!(r.truncated);
        // sorted file order: both hits in f0, first hit in f1
        assert_eq!(r.matches[0].path, "f0.txt");
        assert_eq!(r.matches[2].path, "f1.txt");
    }

    #[test]
    fn binary_and_gitignored_files_are_skipped() {
        let (dir, root) = setup();
        std::fs::write(dir.path().join("b.bin"), b"needle\x00").unwrap();
        // gitignore semantics apply inside git repos (ripgrep behavior)
        write(dir.path(), ".git/HEAD", "ref: refs/heads/main\n");
        write(dir.path(), ".gitignore", "secret.txt\n");
        write(dir.path(), "secret.txt", "needle\n");
        write(dir.path(), "plain.txt", "needle\n");
        let r = search(&root, &params("needle"), &Limits::default()).unwrap();
        let paths: Vec<_> = r.matches.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["plain.txt"]);
    }

    #[test]
    fn no_matches_is_empty_not_error() {
        let (dir, root) = setup();
        write(dir.path(), "f.txt", "nothing here\n");
        let r = search(&root, &params("absent_zzz"), &Limits::default()).unwrap();
        assert!(r.matches.is_empty());
        assert!(!r.truncated);
        // A genuine no-match: files were read, so there is nothing to explain.
        assert_eq!(r.files_searched, 1);
        assert!(r.reason.is_none());
    }

    /// korg #1066, reproduced as it actually happened: a `path`-scoped
    /// search with a bare `glob` that can only match at the root's top
    /// level. The two zeros must stop being the same answer.
    #[test]
    fn a_glob_that_cannot_match_under_path_says_so_instead_of_returning_a_bare_zero() {
        let (dir, root) = setup();
        write(
            dir.path(),
            "ai/kaed/README.md",
            "the journal records edits\n",
        );
        write(dir.path(), "ai/kaed/src/main.rs", "fn main() {}\n");

        let trap = search(
            &root,
            &SearchParams {
                path: "ai/kaed",
                glob: Some("README.md"),
                ..params("journal")
            },
            &Limits::default(),
        )
        .unwrap();
        assert!(trap.matches.is_empty());
        assert_eq!(trap.files_searched, 0, "nothing was opened at all");
        let reason = trap.reason.expect("a zero this shape must explain itself");
        assert_eq!(reason.code, "glob_matched_no_files");
        // The hint names the fix, not the rule.
        assert!(reason.hint.contains("ai/kaed/README.md"), "{}", reason.hint);
        assert!(reason.hint.contains("**/README.md"), "{}", reason.hint);

        // …and the fix the hint suggests actually works.
        let fixed = search(
            &root,
            &SearchParams {
                path: "ai/kaed",
                glob: Some("**/README.md"),
                ..params("journal")
            },
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(fixed.matches.len(), 1);
        assert_eq!(fixed.files_searched, 1);
        assert!(fixed.reason.is_none());
    }

    #[test]
    fn an_empty_subtree_is_distinguishable_from_an_unmatched_glob() {
        let (dir, root) = setup();
        std::fs::create_dir_all(dir.path().join("empty")).unwrap();
        write(dir.path(), "other/f.txt", "hit\n");
        let r = search(
            &root,
            &SearchParams {
                path: "empty",
                ..params("hit")
            },
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.files_searched, 0);
        assert_eq!(r.reason.unwrap().code, "no_files_under_path");
    }

    #[test]
    fn a_match_set_emptied_by_binary_and_size_skips_says_which() {
        let (dir, root) = setup();
        std::fs::write(dir.path().join("b.bin"), b"needle\x00").unwrap();
        let r = search(
            &root,
            &SearchParams {
                glob: Some("*.bin"),
                ..params("needle")
            },
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.files_searched, 0);
        assert_eq!(r.reason.unwrap().code, "all_files_skipped");
    }

    #[test]
    fn bad_pattern_is_invalid_input() {
        let (_dir, root) = setup();
        let err = search(&root, &params("(unclosed"), &Limits::default()).unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
    }

    // ------------------------------------------------ secrets model (008)

    fn classified_root(dir: &Path) -> ResolvedRoot {
        ResolvedRoot::with_default_classify("t", dir.canonicalize().unwrap())
    }

    /// D-8: search runs over the redacted rendering, so a probe for a
    /// secret's VALUE matches nothing — the oracle is dead by construction,
    /// not by result filtering.
    #[test]
    fn classified_dotenv_is_searched_redacted_and_value_probes_find_nothing() {
        let (dir, _) = setup();
        const VALUE: &str = "b7f3a9d2c8e14f60b7f3a9d2c8e14f60";
        write(
            dir.path(),
            ".env",
            &format!("KLAMS_TOKEN={VALUE}\nDEBUG=true\n"),
        );
        write(
            dir.path(),
            "notes.md",
            &format!("the value {VALUE} leaked here\n"),
        );
        let root = classified_root(dir.path());

        // probing for the value: only the plain file answers
        let probe = search(&root, &params(VALUE), &Limits::default()).unwrap();
        let paths: Vec<_> = probe.matches.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["notes.md"], "the .env was searched, redacted");
        assert_eq!(probe.files_searched, 2, "the .env WAS opened and searched");

        // searching for the key: the hit shows the placeholder, never the
        // value, and carries the RAW file's version as its edit base
        let by_key = search(&root, &params("KLAMS_TOKEN"), &Limits::default()).unwrap();
        let hit = by_key
            .matches
            .iter()
            .find(|m| m.path == ".env")
            .expect("keys are searchable");
        assert!(hit.text.contains("⟨kaed:KLAMS_TOKEN@"), "{}", hit.text);
        assert!(!hit.text.contains(VALUE));
        let raw = std::fs::read(dir.path().join(".env")).unwrap();
        assert_eq!(hit.version, fsops::version_of(&raw));
    }

    #[test]
    fn opaque_classified_files_are_skipped_whole_and_counted() {
        let (dir, _) = setup();
        write(
            dir.path(),
            "server.pem",
            "-----BEGIN CERTIFICATE-----\nneedle\n",
        );
        write(dir.path(), "plain.txt", "needle\n");
        let root = classified_root(dir.path());
        let r = search(&root, &params("needle"), &Limits::default()).unwrap();
        let paths: Vec<_> = r.matches.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["plain.txt"]);
        assert_eq!(r.classified_hidden, 1);
        assert_eq!(r.files_searched, 1);
    }

    #[test]
    fn marker_files_are_skipped_and_counted_with_the_denied() {
        let (dir, root) = setup();
        write(dir.path(), "opted-out.txt", "# kaedignore\nneedle\n");
        write(dir.path(), "plain.txt", "needle\n");
        let r = search(&root, &params("needle"), &Limits::default()).unwrap();
        let paths: Vec<_> = r.matches.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["plain.txt"]);
        assert_eq!(r.denied_hidden, 1);
    }

    #[test]
    fn kaedignore_denied_files_are_not_searched() {
        let (dir, root) = setup();
        write(dir.path(), ".kaedignore", "private/\n");
        write(dir.path(), "private/notes.md", "needle\n");
        write(dir.path(), "plain.txt", "needle\n");
        let r = search(&root, &params("needle"), &Limits::default()).unwrap();
        let paths: Vec<_> = r.matches.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["plain.txt"]);
        assert_eq!(r.denied_hidden, 1);
    }

    // ------------------------------------------ the OS layer (014, #1088)

    /// Set up `dir/locked/` as `drwx------ root root` is on kubsdb: present,
    /// undescendable. Returns the restore closure — the tempdir cannot
    /// clean itself up otherwise.
    fn lock_dir(dir: &Path, rel: &str) -> impl FnOnce() {
        use std::os::unix::fs::PermissionsExt;
        let p = dir.join(rel);
        std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o000)).unwrap();
        move || {
            let _ = std::fs::set_permissions(&p, std::fs::Permissions::from_mode(0o755));
        }
    }

    /// korg #1088, reproduced exactly: ONE unreadable directory inside the
    /// root killed the entire call, so the broad root sprint 013 shipped was
    /// unsearchable without a `path` scope. It must degrade the way every
    /// other filter does — skip, count, keep going.
    #[test]
    fn an_unreadable_directory_is_skipped_and_counted_not_fatal() {
        if crate::perm::running_as_root() {
            return;
        }
        let (dir, root) = setup();
        write(dir.path(), "prometheus/prometheus.yml", "needle here\n");
        std::fs::create_dir_all(dir.path().join("lost+found")).unwrap();
        write(dir.path(), "lost+found/inner.txt", "needle\n");
        let restore = lock_dir(dir.path(), "lost+found");

        let r = search(&root, &params("needle"), &Limits::default()).unwrap();
        restore();

        let paths: Vec<_> = r.matches.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["prometheus/prometheus.yml"]);
        assert_eq!(
            r.unreadable_hidden, 1,
            "the skip is reported, or the result silently lies about its own coverage"
        );
        // …and it is NOT conflated with policy: nothing here is denied.
        assert_eq!(r.denied_hidden, 0);
    }

    /// The sibling case: the file is reachable but not openable (a 0600
    /// root-owned config). Previously a silent `continue`, which is the same
    /// R7 dishonesty one level down.
    #[test]
    fn an_unreadable_file_is_counted_too() {
        if crate::perm::running_as_root() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let (dir, root) = setup();
        write(dir.path(), "plain.txt", "needle\n");
        write(dir.path(), "postgresql/docker-compose.yml", "needle\n");
        let locked = dir.path().join("postgresql/docker-compose.yml");
        std::fs::set_permissions(&locked, std::fs::Permissions::from_mode(0o000)).unwrap();

        let r = search(&root, &params("needle"), &Limits::default()).unwrap();
        let paths: Vec<_> = r.matches.iter().map(|m| m.path.as_str()).collect();
        assert_eq!(paths, ["plain.txt"]);
        assert_eq!(r.unreadable_hidden, 1);
        assert_eq!(r.files_searched, 1);
    }

    /// D-6's other half. Skip-and-count is the right answer for what a
    /// walk *finds*; for what the caller *named*, it is the #1091
    /// complaint wearing a different hat — a zero with a count beside it
    /// and no reason, no owner and no route.
    #[test]
    fn an_addressed_unreadable_path_refuses_instead_of_counting() {
        if crate::perm::running_as_root() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let (dir, root) = setup();
        write(dir.path(), "prometheus/prometheus.yml", "needle\n");
        std::fs::set_permissions(
            dir.path().join("prometheus/prometheus.yml"),
            std::fs::Permissions::from_mode(0o000),
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("locked")).unwrap();
        let restore = lock_dir(dir.path(), "locked");

        for (path, subject) in [("prometheus/prometheus.yml", "path"), ("locked", "path")] {
            let err = search(
                &root,
                &SearchParams {
                    path,
                    ..params("needle")
                },
                &Limits::default(),
            )
            .unwrap_err();
            let v = serde_json::to_value(&err).unwrap();
            assert_eq!(v["code"], "denied", "{path}");
            assert_eq!(v["data"]["reason"], "not_readable_by_service_identity");
            assert_eq!(v["data"][subject], path);
        }
        restore();
    }

    /// What the 014 live test found by getting three different numbers for
    /// the same root (1, 2, then 6) across three sessions.
    ///
    /// The hidden counters describe **what the walk actually reached**, so
    /// a search that fills `max_results` stops early and reports less than
    /// the whole picture. That is not a contract gap — `truncated: true`
    /// rides the same response and says the result is partial — but the
    /// counters are **lower bounds** in that case, and reading one without
    /// the other is how three sessions disagreed.
    ///
    /// The split is structural: the walk runs to exhaustion before any
    /// file is opened, so whatever it filters is counted in full, while
    /// everything discovered by *opening* a file (unreadable, classified,
    /// in-file marker) is only counted as far as the budget allowed.
    #[test]
    fn a_truncated_search_reports_lower_bound_counters_and_says_it_is_truncated() {
        if crate::perm::running_as_root() {
            return;
        }
        use std::os::unix::fs::PermissionsExt;
        let (dir, _) = setup();
        // Interleave so a small budget stops before the later ones. Sorted
        // file order makes this deterministic.
        for i in 0..6 {
            write(dir.path(), &format!("{i}-hit.txt"), "needle\n");
            write(dir.path(), &format!("{i}-locked.env"), "TOKEN=needle\n");
            std::fs::set_permissions(
                dir.path().join(format!("{i}-locked.env")),
                std::fs::Permissions::from_mode(0o000),
            )
            .unwrap();
        }
        let root = ResolvedRoot::with_default_classify("t", dir.path().canonicalize().unwrap());

        let full = search(&root, &params("needle"), &Limits::default()).unwrap();
        assert!(!full.truncated);
        assert_eq!(full.unreadable_hidden, 6, "the complete walk sees them all");

        let capped = search(
            &root,
            &SearchParams {
                max_results: 2,
                ..params("needle")
            },
            &Limits::default(),
        )
        .unwrap();
        assert!(capped.truncated, "the partial answer says it is partial");
        assert!(
            capped.unreadable_hidden < full.unreadable_hidden,
            "a truncated walk reports a lower bound, not the whole: {} vs {}",
            capped.unreadable_hidden,
            full.unreadable_hidden
        );
    }

    /// Zero is the common case and must stay off the wire, like its
    /// siblings — a field that is always present stops being read.
    #[test]
    fn nothing_unreadable_means_no_field_at_all() {
        let (dir, root) = setup();
        write(dir.path(), "f.txt", "needle\n");
        let r = search(&root, &params("needle"), &Limits::default()).unwrap();
        let v = serde_json::to_value(&r).unwrap();
        assert!(v.get("unreadable_hidden").is_none());
    }

    #[test]
    fn adjacent_matches_share_interleaved_context() {
        let (dir, root) = setup();
        write(dir.path(), "f.txt", "a\nhit1\nb\nhit2\nc\n");
        let r = search(
            &root,
            &SearchParams {
                context: 1,
                ..params("hit")
            },
            &Limits::default(),
        )
        .unwrap();
        assert_eq!(r.matches.len(), 2);
        assert_eq!(r.matches[0].before, vec!["a"]);
        assert_eq!(r.matches[0].after, vec!["b"]);
        // "b" was already emitted as after-context of hit1; hit2 still
        // gets its own after
        assert_eq!(r.matches[1].after, vec!["c"]);
    }
}
