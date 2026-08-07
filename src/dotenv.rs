//! Strict dotenv: parse, line-preserving redaction, and the typed ops.
//!
//! The grammar is deliberately narrow — every line must be blank, a `#`
//! comment, or `KEY=value` at column 0 (optionally `export KEY=value`).
//! Anything else means the file is **not** dotenv and gets no redacted
//! surface (D-2). Strictness is the gate that keeps redaction honest: a
//! PEM cannot pass it, and neither can a multi-line quoted value that
//! would break the line-preserving property (D-7).
//!
//! Redaction never touches disk. The rendering is a *view*: entry values
//! are replaced in place by placeholders, comment tokens are scanned by
//! the shape detector, and the line count always equals the raw file's —
//! which is what keeps ranges, windows, anchors and search line numbers
//! valid over redacted text.

use crate::errors::{KaedError, Result};
use crate::secrets;
use schemars::JsonSchema;
use serde::Serialize;

#[derive(Debug, Clone)]
pub struct DotenvFile {
    lines: Vec<Line>,
    trailing_newline: bool,
}

#[derive(Debug, Clone)]
struct Line {
    /// Exact original text (no terminator). Untouched lines round-trip
    /// byte-exact; only lines an op modified are re-rendered.
    raw: String,
    kind: LineKind,
}

#[derive(Debug, Clone)]
enum LineKind {
    Blank,
    Comment,
    Entry(Entry),
}

#[derive(Debug, Clone)]
pub struct Entry {
    pub export: bool,
    pub key: String,
    /// Quote-stripped. kaed never interprets escapes inside quotes; the
    /// value is what sits between the delimiters.
    pub value: String,
}

/// The redacted per-entry view returned by `read` (WI #1048's
/// `{key, comment, placeholder, meta}` contract).
#[derive(Debug, Serialize, JsonSchema)]
pub struct EntryView {
    pub key: String,
    /// Absent for empty values — that a key is unset is not a secret.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub placeholder: Option<String>,
    /// The comment block directly above the entry, redacted.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub comment: Option<String>,
    pub meta: EntryMeta,
}

#[derive(Debug, Serialize, JsonSchema)]
pub struct EntryMeta {
    /// Character count of the value.
    pub len: usize,
    /// Disclosable shape: `empty`, `uuid`, `hex`, `base64`,
    /// `prefixed:<tag>`, or `text`. The non-random parts of a shape are
    /// disclosable; the random parts never are.
    pub shape: String,
}

fn is_valid_key(key: &str) -> bool {
    let mut chars = key.chars();
    matches!(chars.next(), Some(c) if c.is_ascii_alphabetic() || c == '_')
        && chars.all(|c| c.is_ascii_alphanumeric() || c == '_')
}

/// Parse `content` under the strict grammar, or `None` if any line falls
/// outside it. `None` is a real answer: the file gets no dotenv surface.
pub fn parse(content: &str) -> Option<DotenvFile> {
    let trailing_newline = content.ends_with('\n');
    let body = if trailing_newline {
        &content[..content.len() - 1]
    } else {
        content
    };
    let mut lines = Vec::new();
    if !body.is_empty() || trailing_newline {
        for raw in body.split('\n') {
            lines.push(Line {
                raw: raw.to_owned(),
                kind: parse_line(raw)?,
            });
        }
    }
    Some(DotenvFile {
        lines,
        trailing_newline,
    })
}

fn parse_line(raw: &str) -> Option<LineKind> {
    if raw.trim().is_empty() {
        return Some(LineKind::Blank);
    }
    if raw.trim_start().starts_with('#') {
        return Some(LineKind::Comment);
    }
    // entries start at column 0 — an indented KEY=value is outside the
    // grammar, as is anything a real dotenv loader would reject
    let (export, rest) = match raw.strip_prefix("export ") {
        Some(r) => (true, r.trim_start()),
        None => (false, raw),
    };
    let eq = rest.find('=')?;
    let key = rest[..eq].trim_end();
    if !is_valid_key(key) {
        return None;
    }
    let value = strip_quotes(rest[eq + 1..].trim());
    Some(LineKind::Entry(Entry {
        export,
        key: key.to_owned(),
        value,
    }))
}

fn strip_quotes(v: &str) -> String {
    let b = v.as_bytes();
    if b.len() >= 2 && (b[0] == b'"' || b[0] == b'\'') && b[b.len() - 1] == b[0] {
        v[1..v.len() - 1].to_owned()
    } else {
        v.to_owned()
    }
}

fn quote_for_render(value: &str) -> String {
    let needs = value
        .chars()
        .any(|c| c.is_whitespace() || matches!(c, '#' | '"' | '\'' | '\\'));
    if needs {
        format!("\"{}\"", value.replace('\\', "\\\\").replace('"', "\\\""))
    } else {
        value.to_owned()
    }
}

fn render_entry(e: &Entry) -> String {
    format!(
        "{}{}={}",
        if e.export { "export " } else { "" },
        e.key,
        if e.value.is_empty() {
            String::new()
        } else {
            quote_for_render(&e.value)
        }
    )
}

impl DotenvFile {
    /// The exact bytes to write back. Untouched lines are the original
    /// bytes; modified lines were re-rendered by the op that touched them.
    pub fn render(&self) -> String {
        let mut s = self
            .lines
            .iter()
            .map(|l| l.raw.as_str())
            .collect::<Vec<_>>()
            .join("\n");
        if self.trailing_newline {
            s.push('\n');
        }
        s
    }

    /// The redacted rendering — same line count as the raw file, always.
    pub fn redact(&self) -> String {
        let mut s = self
            .lines
            .iter()
            .map(|l| match &l.kind {
                LineKind::Blank => l.raw.clone(),
                LineKind::Comment => secrets::redact_free_text(&l.raw),
                LineKind::Entry(e) => format!(
                    "{}{}={}",
                    if e.export { "export " } else { "" },
                    e.key,
                    if e.value.is_empty() {
                        String::new()
                    } else {
                        secrets::placeholder(&e.key, &e.value)
                    }
                ),
            })
            .collect::<Vec<_>>()
            .join("\n");
        if self.trailing_newline {
            s.push('\n');
        }
        s
    }

    pub fn entries(&self) -> Vec<EntryView> {
        self.lines
            .iter()
            .enumerate()
            .filter_map(|(i, l)| match &l.kind {
                LineKind::Entry(e) => Some(EntryView {
                    key: e.key.clone(),
                    placeholder: (!e.value.is_empty())
                        .then(|| secrets::placeholder(&e.key, &e.value)),
                    comment: self.attached_comment_text(i),
                    meta: EntryMeta {
                        len: e.value.chars().count(),
                        shape: secrets::shape_of(&e.value),
                    },
                }),
                _ => None,
            })
            .collect()
    }

    pub fn get(&self, key: &str) -> Option<&Entry> {
        self.lines.iter().find_map(|l| match &l.kind {
            LineKind::Entry(e) if e.key == key => Some(e),
            _ => None,
        })
    }

    pub fn keys(&self) -> Vec<String> {
        self.lines
            .iter()
            .filter_map(|l| match &l.kind {
                LineKind::Entry(e) => Some(e.key.clone()),
                _ => None,
            })
            .collect()
    }

    fn entry_index(&self, key: &str) -> Option<usize> {
        self.lines
            .iter()
            .position(|l| matches!(&l.kind, LineKind::Entry(e) if e.key == key))
    }

    /// Contiguous comment lines directly above line `i` (no blank between):
    /// the entry's attached block. It moves and dies with the entry.
    fn attached_block_start(&self, i: usize) -> usize {
        let mut start = i;
        while start > 0 && matches!(self.lines[start - 1].kind, LineKind::Comment) {
            start -= 1;
        }
        start
    }

    fn attached_comment_text(&self, i: usize) -> Option<String> {
        let start = self.attached_block_start(i);
        if start == i {
            return None;
        }
        Some(
            self.lines[start..i]
                .iter()
                .map(|l| {
                    secrets::redact_free_text(l.raw.trim_start().trim_start_matches('#').trim())
                })
                .collect::<Vec<_>>()
                .join("\n"),
        )
    }

    /// Set `key` to `value` (and/or replace its comment block). Absent key:
    /// appended at the end, `value` required. Present key with `value:
    /// None`: comment-only edit, value untouched.
    pub fn set(&mut self, key: &str, value: Option<&str>, comment: Option<&str>) -> Result<()> {
        if let Some(v) = value
            && (v.contains('\n') || v.contains('\r'))
        {
            // a multi-line value would break both the grammar and the
            // line-preserving redaction (D-7)
            return Err(KaedError::invalid_input(format!(
                "env_set {key:?}: dotenv values must be single-line"
            )));
        }
        match self.entry_index(key) {
            Some(i) => {
                if let Some(v) = value
                    && let LineKind::Entry(e) = &mut self.lines[i].kind
                {
                    e.value = v.to_owned();
                    let raw = render_entry(e);
                    self.lines[i].raw = raw;
                }
                if let Some(c) = comment {
                    let start = self.attached_block_start(i);
                    let block = comment_lines(c);
                    self.lines.splice(start..i, block);
                }
                Ok(())
            }
            None => {
                if !is_valid_key(key) {
                    return Err(KaedError::invalid_input(format!(
                        "{key:?} is not a valid dotenv key ([A-Za-z_][A-Za-z0-9_]*)"
                    )));
                }
                let Some(v) = value else {
                    return Err(KaedError::invalid_input(format!(
                        "env_set of new key {key:?} needs a value"
                    )));
                };
                if let Some(c) = comment {
                    self.lines.extend(comment_lines(c));
                }
                let e = Entry {
                    export: false,
                    key: key.to_owned(),
                    value: v.to_owned(),
                };
                self.lines.push(Line {
                    raw: render_entry(&e),
                    kind: LineKind::Entry(e),
                });
                self.trailing_newline = true;
                Ok(())
            }
        }
    }

    /// Rename `from` to `to`; the value rides along unchanged, so its
    /// digest — and any placeholder equality — survives the rename.
    pub fn rename(&mut self, from: &str, to: &str) -> Result<()> {
        if !is_valid_key(to) {
            return Err(KaedError::invalid_input(format!(
                "{to:?} is not a valid dotenv key ([A-Za-z_][A-Za-z0-9_]*)"
            )));
        }
        if self.entry_index(to).is_some() {
            return Err(KaedError::invalid_input(format!(
                "env_rename target {to:?} already exists"
            )));
        }
        let i = self
            .entry_index(from)
            .ok_or_else(|| KaedError::invalid_input(format!("env_rename: no key {from:?}")))?;
        if let LineKind::Entry(e) = &mut self.lines[i].kind {
            e.key = to.to_owned();
            let raw = render_entry(e);
            self.lines[i].raw = raw;
        }
        Ok(())
    }

    /// Delete `key` and its attached comment block.
    pub fn delete(&mut self, key: &str) -> Result<()> {
        let i = self
            .entry_index(key)
            .ok_or_else(|| KaedError::invalid_input(format!("env_delete: no key {key:?}")))?;
        let start = self.attached_block_start(i);
        self.lines.drain(start..=i);
        Ok(())
    }

    /// Rearrange entries into `keys` order. Entry *positions* (and every
    /// blank and unattached comment line) stay where they were; only which
    /// entry-plus-attached-comments group occupies each position changes.
    /// `keys` must be an exact permutation of the current keys.
    pub fn reorder(&mut self, keys: &[String]) -> Result<()> {
        let current = self.keys();
        let mut want = keys.to_vec();
        let mut have = current.clone();
        want.sort();
        have.sort();
        if have.windows(2).any(|w| w[0] == w[1]) {
            return Err(KaedError::invalid_input(
                "env_reorder: the file has duplicate keys; resolve those first",
            ));
        }
        if want != have {
            return Err(KaedError::invalid_input(format!(
                "env_reorder keys must be exactly the current keys {current:?} in a new order"
            )));
        }
        // Slice the file into free lines and entry groups, in order.
        let mut groups: std::collections::BTreeMap<String, Vec<Line>> =
            std::collections::BTreeMap::new();
        let mut sequence = Vec::new(); // None = the next group slot, Some = a free line
        let mut i = 0;
        while i < self.lines.len() {
            if let LineKind::Entry(e) = &self.lines[i].kind {
                let start = self.attached_block_start(i);
                // attached comments already consumed as free lines? undo:
                for _ in start..i {
                    sequence.pop();
                }
                groups.insert(e.key.clone(), self.lines[start..=i].to_vec());
                sequence.push(None);
            } else {
                sequence.push(Some(self.lines[i].clone()));
            }
            i += 1;
        }
        let mut ordered = keys.iter();
        let mut out = Vec::with_capacity(self.lines.len());
        for item in sequence {
            match item {
                Some(line) => out.push(line),
                None => {
                    let key = ordered.next().expect("one slot per group");
                    out.extend(groups[key].iter().cloned());
                }
            }
        }
        self.lines = out;
        Ok(())
    }
}

fn comment_lines(text: &str) -> Vec<Line> {
    if text.is_empty() {
        return Vec::new();
    }
    text.lines()
        .map(|l| Line {
            raw: format!("# {l}"),
            kind: LineKind::Comment,
        })
        .collect()
}

/// Resolve a value an op supplied: a literal passes through; a string that
/// is exactly a placeholder resolves to the real value it seals (from any
/// key in `file` — copying a value between keys is the equality use case);
/// anything merely containing placeholder syntax is refused, so a
/// truncated placeholder can never land as a literal.
pub fn resolve_value(file: &DotenvFile, path: &str, raw: &str) -> Result<String> {
    if let Some(ph) = secrets::parse_placeholder(raw) {
        let entry = file.get(ph.key).ok_or_else(|| {
            KaedError::invalid_input(format!(
                "placeholder names key {:?}, which does not exist in {path}",
                ph.key
            ))
        })?;
        if let Some(d) = ph.digest
            && secrets::digest_of(&entry.value) != d
        {
            // PD-3: exact-vs-current must never be ambiguous. A digest that
            // no longer matches fails loudly instead of silently resolving
            // to whatever the value is now.
            return Err(KaedError::invalid_input(format!(
                "placeholder digest mismatch for {:?} in {path}: the value changed since \
                 it was read — re-read the file and pass the current placeholder",
                ph.key
            )));
        }
        return Ok(entry.value.clone());
    }
    if secrets::contains_placeholder_syntax(raw) {
        return Err(KaedError::invalid_input(
            "value contains kaed placeholder syntax but is not exactly one placeholder — \
             placeholders are sealed handles, passed through whole or not at all",
        ));
    }
    Ok(raw.to_owned())
}

/// Keys in `before` whose non-empty value survives nowhere in `after` —
/// the values a write would destroy. Value-presence, not key-presence: a
/// rename or a copy preserves the value and trips nothing; only actual
/// destruction (delete, or overwrite by a new literal) shows up here.
pub fn vanished_values(before: Option<&DotenvFile>, after: Option<&DotenvFile>) -> Vec<String> {
    let Some(before) = before else {
        return Vec::new();
    };
    let after_values: std::collections::BTreeSet<&str> = after
        .map(|f| {
            f.lines
                .iter()
                .filter_map(|l| match &l.kind {
                    LineKind::Entry(e) => Some(e.value.as_str()),
                    _ => None,
                })
                .collect()
        })
        .unwrap_or_default();
    let mut gone: Vec<String> = before
        .lines
        .iter()
        .filter_map(|l| match &l.kind {
            LineKind::Entry(e)
                if !e.value.is_empty() && !after_values.contains(e.value.as_str()) =>
            {
                Some(e.key.clone())
            }
            _ => None,
        })
        .collect();
    gone.sort();
    gone.dedup();
    gone
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = "\
# service credentials
# rotated 2026-08-01
KLAMS_TOKEN=b7f3a9d2c8e14f60b7f3a9d2c8e14f60

DEBUG=true
export DATABASE_URL=\"postgres://korg:korg@localhost/korg\"
EMPTY=
";

    #[test]
    fn strict_grammar_accepts_dotenv_and_rejects_everything_else() {
        assert!(parse(SAMPLE).is_some());
        assert!(parse("").is_some(), "empty file is trivially dotenv");
        assert!(parse("# only comments\n\n").is_some());

        for not_dotenv in [
            "-----BEGIN CERTIFICATE-----\nMIIB\n-----END CERTIFICATE-----\n",
            "key: value\n",                   // yaml
            "  INDENTED=1\n",                 // entries start at column 0
            "2KEY=x\n",                       // invalid key
            "MULTI=\"line one\nline two\"\n", // would break line-preservation
        ] {
            assert!(parse(not_dotenv).is_none(), "{not_dotenv:?}");
        }
    }

    #[test]
    fn render_round_trips_byte_exact() {
        let f = parse(SAMPLE).unwrap();
        assert_eq!(f.render(), SAMPLE);
        let no_trailing = "A=1";
        assert_eq!(parse(no_trailing).unwrap().render(), no_trailing);
    }

    #[test]
    fn redaction_is_line_preserving_and_leaks_no_value() {
        let f = parse(SAMPLE).unwrap();
        let red = f.redact();
        assert_eq!(red.lines().count(), SAMPLE.lines().count());
        assert!(!red.contains("b7f3a9d2c8e14f60b7f3a9d2c8e14f60"));
        assert!(!red.contains("postgres://korg:korg"));
        // high-entropy value: digest disclosed
        assert!(red.contains("⟨kaed:KLAMS_TOKEN@"), "{red}");
        // below the floor: digest withheld (PD-2)
        assert!(red.contains("⟨kaed:DEBUG⟩"), "{red}");
        // empty value renders plainly — unset is not a secret
        assert!(red.contains("EMPTY=\n"), "{red}");
        // export prefix survives
        assert!(
            red.contains("export DATABASE_URL=⟨kaed:DATABASE_URL"),
            "{red}"
        );
        // comments survive (nothing secret-shaped in these)
        assert!(red.contains("# service credentials"));
    }

    #[test]
    fn entries_view_carries_key_comment_placeholder_meta() {
        let f = parse(SAMPLE).unwrap();
        let v = f.entries();
        assert_eq!(v.len(), 4);
        assert_eq!(v[0].key, "KLAMS_TOKEN");
        assert_eq!(
            v[0].comment.as_deref(),
            Some("service credentials\nrotated 2026-08-01")
        );
        assert_eq!(v[0].meta.shape, "hex");
        assert_eq!(v[0].meta.len, 32);
        assert!(v[0].placeholder.as_deref().unwrap().contains('@'));
        assert_eq!(v[1].key, "DEBUG");
        assert!(v[1].comment.is_none(), "blank line detaches the block");
        assert_eq!(v[3].key, "EMPTY");
        assert!(v[3].placeholder.is_none());
        assert_eq!(v[3].meta.shape, "empty");
    }

    #[test]
    fn set_updates_appends_and_handles_comments() {
        let mut f = parse(SAMPLE).unwrap();
        f.set("DEBUG", Some("false"), None).unwrap();
        assert_eq!(f.get("DEBUG").unwrap().value, "false");

        f.set("NEW_KEY", Some("with space"), Some("added by test"))
            .unwrap();
        let out = f.render();
        assert!(
            out.contains("# added by test\nNEW_KEY=\"with space\""),
            "{out}"
        );

        assert!(
            f.set("ALSO_NEW", None, None).is_err(),
            "new key needs a value"
        );
        assert!(f.set("2bad", Some("x"), None).is_err());

        // comment-only edit leaves the value alone
        f.set("KLAMS_TOKEN", None, Some("rotated again")).unwrap();
        assert_eq!(
            f.get("KLAMS_TOKEN").unwrap().value,
            "b7f3a9d2c8e14f60b7f3a9d2c8e14f60"
        );
        assert!(f.render().contains("# rotated again\nKLAMS_TOKEN="));
        assert!(!f.render().contains("rotated 2026-08-01"), "block replaced");
    }

    #[test]
    fn rename_preserves_the_value_and_delete_takes_the_block() {
        let mut f = parse(SAMPLE).unwrap();
        f.rename("KLAMS_TOKEN", "KLAMS_API_TOKEN").unwrap();
        assert_eq!(
            f.get("KLAMS_API_TOKEN").unwrap().value,
            "b7f3a9d2c8e14f60b7f3a9d2c8e14f60"
        );
        assert!(f.rename("NOPE", "X").is_err());
        assert!(f.rename("DEBUG", "EMPTY").is_err(), "target exists");

        f.delete("KLAMS_API_TOKEN").unwrap();
        let out = f.render();
        assert!(!out.contains("KLAMS_API_TOKEN"));
        assert!(
            !out.contains("# service credentials"),
            "attached block goes too"
        );
        assert!(f.delete("KLAMS_API_TOKEN").is_err(), "already gone");
    }

    #[test]
    fn reorder_permutes_groups_but_keeps_positions() {
        let mut f = parse(SAMPLE).unwrap();
        f.reorder(&[
            "EMPTY".into(),
            "DATABASE_URL".into(),
            "DEBUG".into(),
            "KLAMS_TOKEN".into(),
        ])
        .unwrap();
        let out = f.render();
        // first group slot now holds EMPTY, with KLAMS_TOKEN's old comment
        // block travelling with KLAMS_TOKEN to the last slot
        assert!(out.starts_with("EMPTY=\n"), "{out}");
        assert!(out.contains("# service credentials\n# rotated 2026-08-01\nKLAMS_TOKEN="));
        assert_eq!(out.lines().count(), SAMPLE.lines().count());

        assert!(
            f.reorder(&["EMPTY".into()]).is_err(),
            "must be a permutation"
        );
    }

    #[test]
    fn resolve_value_passes_literals_and_resolves_placeholders() {
        let f = parse(SAMPLE).unwrap();
        assert_eq!(resolve_value(&f, ".env", "literal").unwrap(), "literal");

        // same-key passthrough
        let ph = secrets::placeholder("KLAMS_TOKEN", "b7f3a9d2c8e14f60b7f3a9d2c8e14f60");
        assert_eq!(
            resolve_value(&f, ".env", &ph).unwrap(),
            "b7f3a9d2c8e14f60b7f3a9d2c8e14f60"
        );
        // cross-key copy: another key's placeholder resolves to its value
        let ph2 = secrets::placeholder("DEBUG", "true");
        assert_eq!(resolve_value(&f, ".env", &ph2).unwrap(), "true");

        // digest mismatch fails loudly (PD-3), never silently resolves
        let stale = "⟨kaed:KLAMS_TOKEN@0000000000000000⟩";
        let err = resolve_value(&f, ".env", stale).unwrap_err();
        assert!(err.message.contains("digest mismatch"), "{}", err.message);

        // unknown key
        assert!(resolve_value(&f, ".env", "⟨kaed:GHOST⟩").is_err());

        // embedded / truncated placeholder syntax is unrepresentable
        let embedded = format!("prefix-{ph}");
        assert!(resolve_value(&f, ".env", &embedded).is_err());
        assert!(resolve_value(&f, ".env", "⟨kaed:KLAMS_TOKEN@trunc").is_err());
    }

    #[test]
    fn vanished_values_reports_destruction_not_movement() {
        let before = parse(SAMPLE).unwrap();

        // rename: value survives under the new key → nothing vanished
        let mut renamed = before.clone();
        renamed.rename("KLAMS_TOKEN", "TOKEN2").unwrap();
        assert!(vanished_values(Some(&before), Some(&renamed)).is_empty());

        // delete: the value is gone
        let mut deleted = before.clone();
        deleted.delete("KLAMS_TOKEN").unwrap();
        assert_eq!(
            vanished_values(Some(&before), Some(&deleted)),
            vec!["KLAMS_TOKEN"]
        );

        // overwrite: the old value is gone even though the key remains
        let mut replaced = before.clone();
        replaced.set("KLAMS_TOKEN", Some("newvalue"), None).unwrap();
        assert_eq!(
            vanished_values(Some(&before), Some(&replaced)),
            vec!["KLAMS_TOKEN"]
        );

        // empty values never count; a created file has no before
        assert!(vanished_values(None, Some(&before)).is_empty());
        // after = None (unparseable replacement): every non-empty value vanished
        let all = vanished_values(Some(&before), None);
        assert_eq!(all, vec!["DATABASE_URL", "DEBUG", "KLAMS_TOKEN"]);
    }
}
