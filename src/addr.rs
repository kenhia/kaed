//! Addressing math: content anchors and line ranges. Pure text functions —
//! no filesystem, no transport — so the edit engine and shaped reads share
//! one set of semantics (R5).
//!
//! Lines are 1-based and ranges inclusive, per the contract.

use crate::errors::{AmbiguousAnchorData, KaedError, Result};

/// A file's content split for line math. Preserves whether the file ended
/// with a newline so `join` round-trips byte-exact.
#[derive(Debug, Clone, PartialEq)]
pub struct Lines {
    lines: Vec<String>,
    trailing_newline: bool,
}

impl Lines {
    pub fn split(content: &str) -> Self {
        if content.is_empty() {
            return Self {
                lines: Vec::new(),
                trailing_newline: false,
            };
        }
        let trailing_newline = content.ends_with('\n');
        let body = if trailing_newline {
            &content[..content.len() - 1]
        } else {
            content
        };
        Self {
            lines: body.split('\n').map(str::to_owned).collect(),
            trailing_newline,
        }
    }

    pub fn join(&self) -> String {
        let mut s = self.lines.join("\n");
        if self.trailing_newline {
            s.push('\n');
        }
        s
    }

    pub fn count(&self) -> usize {
        self.lines.len()
    }

    pub fn as_slice(&self) -> &[String] {
        &self.lines
    }

    /// Replace inclusive 1-based `start..=end` with `new_text`'s lines.
    /// Empty `new_text` removes the range; one trailing newline on
    /// `new_text` is not an extra empty line.
    pub fn replace_range(&mut self, start: usize, end: usize, new_text: &str) -> Result<()> {
        self.check_range(start, end)?;
        let replacement = replacement_lines(new_text);
        self.lines.splice(start - 1..end, replacement);
        Ok(())
    }

    pub fn check_range(&self, start: usize, end: usize) -> Result<()> {
        if start == 0 || end < start {
            return Err(KaedError::invalid_input(format!(
                "invalid range {start}..{end}: lines are 1-based and ranges inclusive"
            )));
        }
        if end > self.count() {
            return Err(KaedError::invalid_input(format!(
                "range {start}..{end} exceeds file length ({} lines)",
                self.count()
            )));
        }
        Ok(())
    }
}

fn replacement_lines(new_text: &str) -> Vec<String> {
    if new_text.is_empty() {
        return Vec::new();
    }
    let body = new_text.strip_suffix('\n').unwrap_or(new_text);
    body.split('\n').map(str::to_owned).collect()
}

/// One anchor match: where it starts, byte-wise and line-wise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AnchorHit {
    pub byte_offset: usize,
    /// 1-based line of the match start.
    pub line: usize,
}

/// All non-overlapping matches of `anchor`, in order.
pub fn find_anchor(content: &str, anchor: &str) -> Vec<AnchorHit> {
    if anchor.is_empty() {
        return Vec::new();
    }
    let mut hits = Vec::new();
    let mut from = 0;
    while let Some(pos) = content[from..].find(anchor) {
        let byte_offset = from + pos;
        hits.push(AnchorHit {
            byte_offset,
            line: line_of_offset(content, byte_offset),
        });
        from = byte_offset + anchor.len();
    }
    hits
}

/// Resolve an anchor to exactly one hit. `occurrence` is 1-based; without
/// it, more than one match is `ambiguous_anchor` (R4: candidates in data —
/// each with its line's content since 022, so the error can be acted on
/// without a second read).
pub fn resolve_anchor(
    content: &str,
    anchor: &str,
    occurrence: Option<usize>,
    path: &str,
) -> Result<AnchorHit> {
    if anchor.is_empty() {
        return Err(KaedError::invalid_input("anchor text must be non-empty"));
    }
    let hits = find_anchor(content, anchor);
    match (hits.len(), occurrence) {
        (0, _) => Err(KaedError::anchor_not_found(path)),
        (1, None) => Ok(hits[0]),
        (_, None) => Err(KaedError::ambiguous_anchor(AmbiguousAnchorData::new(
            path,
            Lines::split(content).as_slice(),
            &hits.iter().map(|h| h.line).collect::<Vec<_>>(),
        ))),
        (n, Some(o)) => {
            if o == 0 || o > n {
                return Err(KaedError::invalid_input(format!(
                    "occurrence {o} out of range: anchor matches {n} time(s) in {path}"
                )));
            }
            Ok(hits[o - 1])
        }
    }
}

/// 1-based line containing `offset`.
pub fn line_of_offset(content: &str, offset: usize) -> usize {
    content[..offset].bytes().filter(|&b| b == b'\n').count() + 1
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorCode;

    #[test]
    fn split_join_round_trips() {
        for content in ["", "a", "a\n", "a\nb", "a\nb\n", "\n", "a\n\nb\n"] {
            assert_eq!(Lines::split(content).join(), content, "{content:?}");
        }
    }

    #[test]
    fn line_counts() {
        assert_eq!(Lines::split("").count(), 0);
        assert_eq!(Lines::split("a").count(), 1);
        assert_eq!(Lines::split("a\n").count(), 1);
        assert_eq!(Lines::split("a\nb").count(), 2);
        assert_eq!(Lines::split("\n").count(), 1);
    }

    #[test]
    fn replace_range_middle() {
        let mut l = Lines::split("a\nb\nc\nd\n");
        l.replace_range(2, 3, "X\nY\nZ").unwrap();
        assert_eq!(l.join(), "a\nX\nY\nZ\nd\n");
    }

    #[test]
    fn replace_range_trailing_newline_in_new_text_is_not_an_extra_line() {
        let mut l = Lines::split("a\nb\nc\n");
        l.replace_range(2, 2, "X\n").unwrap();
        assert_eq!(l.join(), "a\nX\nc\n");
    }

    #[test]
    fn replace_range_empty_new_text_deletes() {
        let mut l = Lines::split("a\nb\nc\n");
        l.replace_range(2, 2, "").unwrap();
        assert_eq!(l.join(), "a\nc\n");
    }

    #[test]
    fn replace_range_whole_file() {
        let mut l = Lines::split("a\nb\n");
        l.replace_range(1, 2, "only").unwrap();
        assert_eq!(l.join(), "only\n");
    }

    #[test]
    fn replace_range_rejects_bad_ranges() {
        let mut l = Lines::split("a\nb\n");
        for (s, e) in [(0, 1), (2, 1), (1, 3), (3, 3)] {
            let err = l.replace_range(s, e, "x").unwrap_err();
            assert_eq!(err.code, ErrorCode::InvalidInput, "range {s}..{e}");
        }
    }

    #[test]
    fn find_anchor_reports_offsets_and_lines() {
        let content = "fn a() {}\nfn b() {}\nfn a() {}\n";
        let hits = find_anchor(content, "fn a");
        assert_eq!(hits.len(), 2);
        assert_eq!(
            hits[0],
            AnchorHit {
                byte_offset: 0,
                line: 1
            }
        );
        assert_eq!(hits[1].line, 3);
    }

    #[test]
    fn find_anchor_non_overlapping() {
        assert_eq!(find_anchor("aaaa", "aa").len(), 2);
    }

    #[test]
    fn resolve_anchor_unique() {
        let hit = resolve_anchor("x\nneedle\ny\n", "needle", None, "f.txt").unwrap();
        assert_eq!(hit.line, 2);
    }

    #[test]
    fn resolve_anchor_not_found() {
        let err = resolve_anchor("abc", "zzz", None, "f.txt").unwrap_err();
        assert_eq!(err.code, ErrorCode::AnchorNotFound);
    }

    /// The whole point of 022 D-1: the payload has to be pickable *from*,
    /// not merely indicative. A line number alone sent agents guessing at a
    /// range around it, which cost two reads instead of one.
    #[test]
    fn resolve_anchor_ambiguous_carries_each_line_content() {
        let err = resolve_anchor("dup here\nx\ndup there\n", "dup", None, "f.txt").unwrap_err();
        assert_eq!(err.code, ErrorCode::AmbiguousAnchor);
        let data = err.data.unwrap();
        assert_eq!(data["total"], 2);
        assert_eq!(data["truncated"], false);
        assert_eq!(
            data["occurrences"],
            serde_json::json!([
                {"line": 1, "text": "dup here"},
                {"line": 3, "text": "dup there"},
            ])
        );
        assert!(
            data["hint"].as_str().unwrap().contains("occurrence"),
            "the hint names the field that resolves this"
        );
    }

    /// Truncation is explicit, never silent — a core invariant — and the
    /// hint changes to name `search`, which answers in one call where a
    /// guessed range costs two.
    #[test]
    fn resolve_anchor_ambiguous_caps_occurrences_and_says_so() {
        let content = "dup\n".repeat(crate::errors::ANCHOR_OCCURRENCE_MAX + 5);
        let err = resolve_anchor(&content, "dup", None, "f.txt").unwrap_err();
        let data = err.data.unwrap();
        assert_eq!(data["total"], crate::errors::ANCHOR_OCCURRENCE_MAX + 5);
        assert_eq!(data["truncated"], true);
        assert_eq!(
            data["occurrences"].as_array().unwrap().len(),
            crate::errors::ANCHOR_OCCURRENCE_MAX
        );
        assert!(data["hint"].as_str().unwrap().contains("search"));
        // and the message says it is showing a subset rather than implying
        // the list is the whole truth
        assert!(err.message.contains("first"));
    }

    /// A minified file's one enormous line must not become the payload.
    #[test]
    fn resolve_anchor_ambiguous_clips_long_lines_on_a_char_boundary() {
        let long = format!("dup{}", "é".repeat(400));
        let err = resolve_anchor(&format!("{long}\n{long}\n"), "dup", None, "f.txt").unwrap_err();
        let data = err.data.unwrap();
        let text = data["occurrences"][0]["text"].as_str().unwrap();
        assert_eq!(
            text.chars().count(),
            crate::errors::ANCHOR_LINE_MAX_CHARS + 1,
            "capped content plus the ellipsis marking the cut"
        );
        assert!(text.ends_with('…'));
    }

    #[test]
    fn resolve_anchor_occurrence_picks() {
        let hit = resolve_anchor("dup\nx\ndup\n", "dup", Some(2), "f.txt").unwrap();
        assert_eq!(hit.line, 3);
    }

    #[test]
    fn resolve_anchor_occurrence_out_of_range() {
        let err = resolve_anchor("dup\nx\ndup\n", "dup", Some(3), "f.txt").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn resolve_anchor_empty_is_invalid() {
        let err = resolve_anchor("abc", "", None, "f.txt").unwrap_err();
        assert_eq!(err.code, ErrorCode::InvalidInput);
    }

    #[test]
    fn line_of_offset_basics() {
        let c = "ab\ncd\nef";
        assert_eq!(line_of_offset(c, 0), 1);
        assert_eq!(line_of_offset(c, 2), 1);
        assert_eq!(line_of_offset(c, 3), 2);
        assert_eq!(line_of_offset(c, 7), 3);
    }
}
