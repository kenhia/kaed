//! Server-side search (ripgrep engine). Each match carries its file's
//! `version`, so search → edit works with no read in between: a stale hit
//! becomes `version_conflict`, never a wrong edit.
//!
//! Files are read once and searched as a slice — the version is hashed
//! from exactly the bytes that were searched. Binary and over-limit files
//! are skipped silently, like ripgrep. File order is sorted, so results
//! and truncation are deterministic.

use crate::config::{Limits, ResolvedRoot};
use crate::errors::{KaedError, Result};
use crate::fsops;
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
    /// Files the deny list kept out of the search entirely. Omitted when
    /// zero — "no matches" and "no matches in what I was allowed to read"
    /// are different answers.
    #[serde(skip_serializing_if = "is_zero")]
    pub denied_hidden: usize,
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
    let mut files: Vec<std::path::PathBuf> = Vec::new();
    if base.is_file() {
        files.push(base);
    } else {
        let walker = {
            let deny = root.deny.clone();
            let counter = denied_hidden.clone();
            ignore::WalkBuilder::new(&base)
                .hidden(false)
                .filter_entry(move |e| {
                    if e.file_name() == ".git" {
                        return false;
                    }
                    if deny.is_denied(e.path()) {
                        counter.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                        return false;
                    }
                    true
                })
                .build()
        };
        for entry in walker {
            let entry = entry.map_err(|e| KaedError::internal(e.to_string()))?;
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
        let Ok(meta) = std::fs::metadata(abs) else {
            continue;
        };
        if meta.len() > limits.max_file_bytes {
            continue;
        }
        let Ok(bytes) = std::fs::read(abs) else {
            continue;
        };
        if fsops::looks_binary(&bytes) {
            continue;
        }
        tally.opened += 1;
        let mut sink = CollectSink {
            matcher: &matcher,
            rel: &rel,
            version: None,
            bytes: &bytes,
            matches: &mut matches,
            before_buf: Vec::new(),
            budget: p.max_results,
            hit_budget: false,
        };
        searcher
            .search_slice(&matcher, &bytes, &mut sink)
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
    })
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
