//! The secret shape registry (#1051): a **closed grammar** of mintable
//! shapes, so `generate`/`rotate` never become an eval-shaped feature.
//!
//! A shape's non-random parts (its spec) are disclosable; its random parts
//! never are. Specs: `hex(N)`, `base64url(N)`, `uuid4`,
//! `prefixed(tag, inner)`. Project-level `[secrets] shapes` entries map a
//! *name* to a spec; `generate` accepts either.
//!
//! Minimum lengths are part of the grammar: every mintable shape clears the
//! PD-2 entropy floor by construction, so a generated value always carries
//! a disclosed digest — which is what `describe`'s handle and `rotate`'s
//! like-for-like detection rely on.

use crate::errors::{KaedError, Result};
use std::fmt;

/// Grammar minimums, chosen so the smallest mintable value clears the
/// 80-bit floor under `secrets::estimated_entropy_bits`.
const MIN_HEX_CHARS: usize = 24;
const MAX_HEX_CHARS: usize = 128;
const MIN_B64_CHARS: usize = 16;
const MAX_B64_CHARS: usize = 128;
const MAX_PREFIX_LEN: usize = 24;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Shape {
    /// `hex(N)`: N lowercase hex characters.
    Hex { chars: usize },
    /// `base64url(N)`: N characters of the URL-safe alphabet, unpadded.
    Base64Url { chars: usize },
    /// `uuid4`: a random RFC 4122 v4 UUID, lowercase.
    Uuid4,
    /// `prefixed(tag, inner)`: a fixed disclosable tag before an inner
    /// shape — the homelab `klams-<hex>` convention. One level only.
    Prefixed { tag: String, inner: Box<Shape> },
}

impl fmt::Display for Shape {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Shape::Hex { chars } => write!(f, "hex({chars})"),
            Shape::Base64Url { chars } => write!(f, "base64url({chars})"),
            Shape::Uuid4 => write!(f, "uuid4"),
            Shape::Prefixed { tag, inner } => write!(f, "prefixed({tag},{inner})"),
        }
    }
}

/// Parse a spec string. The grammar is closed and the error names it whole,
/// so a wrong spec is a one-round-trip fix.
pub fn parse_spec(s: &str) -> Result<Shape> {
    parse_inner(s.trim(), true)
}

fn grammar_err(s: &str, detail: &str) -> KaedError {
    KaedError::invalid_input(format!(
        "shape spec {s:?}: {detail}. The grammar is hex(N) [{MIN_HEX_CHARS}..={MAX_HEX_CHARS}], \
         base64url(N) [{MIN_B64_CHARS}..={MAX_B64_CHARS}], uuid4, or prefixed(tag,inner) — \
         or a name from [secrets] shapes"
    ))
}

fn parse_inner(s: &str, allow_prefixed: bool) -> Result<Shape> {
    if s == "uuid4" {
        return Ok(Shape::Uuid4);
    }
    if let Some(n) = arg_of(s, "hex") {
        let chars: usize = n.parse().map_err(|_| grammar_err(s, "N is not a number"))?;
        if !(MIN_HEX_CHARS..=MAX_HEX_CHARS).contains(&chars) {
            return Err(grammar_err(s, "N outside the allowed range"));
        }
        return Ok(Shape::Hex { chars });
    }
    if let Some(n) = arg_of(s, "base64url") {
        let chars: usize = n.parse().map_err(|_| grammar_err(s, "N is not a number"))?;
        if !(MIN_B64_CHARS..=MAX_B64_CHARS).contains(&chars) {
            return Err(grammar_err(s, "N outside the allowed range"));
        }
        return Ok(Shape::Base64Url { chars });
    }
    if let Some(args) = arg_of(s, "prefixed") {
        if !allow_prefixed {
            return Err(grammar_err(s, "prefixed cannot nest"));
        }
        let (tag, inner) = args
            .split_once(',')
            .ok_or_else(|| grammar_err(s, "prefixed takes (tag, inner)"))?;
        let tag = tag.trim();
        if tag.is_empty()
            || tag.len() > MAX_PREFIX_LEN
            || !tag
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
        {
            return Err(grammar_err(s, "tag must be 1-24 chars of [A-Za-z0-9_-]"));
        }
        let inner = parse_inner(inner.trim(), false)?;
        return Ok(Shape::Prefixed {
            tag: tag.to_owned(),
            inner: Box::new(inner),
        });
    }
    Err(grammar_err(s, "not a known shape"))
}

/// `name(arg)` → `arg`, or None if `s` is not that call shape.
fn arg_of<'a>(s: &'a str, name: &str) -> Option<&'a str> {
    s.strip_prefix(name)?
        .strip_prefix('(')?
        .strip_suffix(')')
        .map(str::trim)
}

impl Shape {
    /// Mint a fresh value from OS entropy. Never logged, never journaled;
    /// the caller writes it and drops it.
    pub fn mint(&self) -> Result<String> {
        match self {
            Shape::Hex { chars } => {
                let bytes = random_bytes(chars.div_ceil(2))?;
                let mut s = hex(&bytes);
                s.truncate(*chars);
                Ok(s)
            }
            Shape::Base64Url { chars } => {
                // 6 bits per output char; over-generate and truncate.
                let bytes = random_bytes(chars.div_ceil(4) * 3 + 3)?;
                let mut s = base64url(&bytes);
                s.truncate(*chars);
                Ok(s)
            }
            Shape::Uuid4 => {
                let mut b = random_bytes(16)?;
                b[6] = (b[6] & 0x0f) | 0x40; // version 4
                b[8] = (b[8] & 0x3f) | 0x80; // RFC 4122 variant
                let h = hex(&b);
                Ok(format!(
                    "{}-{}-{}-{}-{}",
                    &h[0..8],
                    &h[8..12],
                    &h[12..16],
                    &h[16..20],
                    &h[20..32]
                ))
            }
            Shape::Prefixed { tag, inner } => Ok(format!("{tag}{}", inner.mint()?)),
        }
    }

    /// The character length of a minted value, for `meta.len` parity checks.
    pub fn minted_len(&self) -> usize {
        match self {
            Shape::Hex { chars } | Shape::Base64Url { chars } => *chars,
            Shape::Uuid4 => 36,
            Shape::Prefixed { tag, inner } => tag.len() + inner.minted_len(),
        }
    }
}

/// Detect the shape of an existing value, for `rotate`'s like-for-like
/// re-mint. **Deliberately conservative**: hex, uuid4 and base64url only.
/// Provider-prefixed tokens are externally issued — kaed cannot mint a
/// valid one, so rotating them by detection would be a lie; and free text
/// has no shape to preserve. Both return `None`, and `rotate` then
/// requires an explicit shape (D-4).
pub fn detect(value: &str) -> Option<Shape> {
    let n = value.chars().count();
    if crate::secrets::provider_prefix(value).is_some() {
        // externally issued: kaed cannot mint a valid one
        return None;
    }
    if is_uuid4(value) {
        return Some(Shape::Uuid4);
    }
    if (MIN_HEX_CHARS..=MAX_HEX_CHARS).contains(&n)
        && value
            .chars()
            .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
    {
        return Some(Shape::Hex { chars: n });
    }
    if (MIN_B64_CHARS..=MAX_B64_CHARS).contains(&n)
        && value
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '-' | '_'))
    {
        return Some(Shape::Base64Url { chars: n });
    }
    None
}

fn is_uuid4(s: &str) -> bool {
    let parts: Vec<&str> = s.split('-').collect();
    parts.len() == 5
        && [8, 4, 4, 4, 12].iter().zip(&parts).all(|(len, p)| {
            p.len() == *len
                && p.chars()
                    .all(|c| c.is_ascii_digit() || ('a'..='f').contains(&c))
        })
        && parts[2].starts_with('4')
}

fn random_bytes(n: usize) -> Result<Vec<u8>> {
    let mut buf = vec![0u8; n];
    getrandom::fill(&mut buf)
        .map_err(|e| KaedError::internal(format!("OS entropy unavailable: {e}")))?;
    Ok(buf)
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

const B64URL: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";

fn base64url(bytes: &[u8]) -> String {
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let b = [
            chunk[0],
            chunk.get(1).copied().unwrap_or(0),
            chunk.get(2).copied().unwrap_or(0),
        ];
        let idx = [
            b[0] >> 2,
            ((b[0] & 0x03) << 4) | (b[1] >> 4),
            ((b[1] & 0x0f) << 2) | (b[2] >> 6),
            b[2] & 0x3f,
        ];
        let take = match chunk.len() {
            1 => 2,
            2 => 3,
            _ => 4,
        };
        for i in &idx[..take] {
            out.push(B64URL[*i as usize] as char);
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::secrets;

    #[test]
    fn specs_round_trip_through_display() {
        for s in [
            "hex(64)",
            "base64url(43)",
            "uuid4",
            "prefixed(klams-,hex(64))",
        ] {
            assert_eq!(parse_spec(s).unwrap().to_string(), s, "{s}");
        }
        // whitespace tolerated on the way in, normalized on the way out
        assert_eq!(
            parse_spec(" prefixed( klams- , hex(64) ) ")
                .unwrap()
                .to_string(),
            "prefixed(klams-,hex(64))"
        );
    }

    #[test]
    fn the_grammar_is_closed() {
        for bad in [
            "hex(4)",                          // below the entropy minimum
            "hex(1000)",                       // above the cap
            "hex(abc)",                        //
            "base64url(8)",                    // below minimum
            "passphrase(4)",                   // deliberately not built (D-4)
            "uuid5",                           //
            "prefixed(klams-)",                // no inner
            "prefixed(k,prefixed(l,hex(24)))", // no nesting
            "prefixed(bad tag!,hex(24))",      //
            "system('rm -rf')",                //
        ] {
            assert!(parse_spec(bad).is_err(), "{bad:?} must not parse");
        }
    }

    #[test]
    fn minted_values_match_their_shape_and_clear_the_floor() {
        for spec in [
            "hex(24)", // the grammar minimum still clears the floor
            "hex(64)",
            "base64url(16)",
            "base64url(43)",
            "uuid4",
            "prefixed(klams-,hex(64))",
        ] {
            let shape = parse_spec(spec).unwrap();
            let v = shape.mint().unwrap();
            assert_eq!(v.chars().count(), shape.minted_len(), "{spec}: {v}");
            assert!(
                secrets::clears_floor(&v),
                "{spec} minted a below-floor value: {v}"
            );
            // like-for-like: what we mint, detection recognizes (except
            // prefixed, which is deliberately undetectable)
            if !spec.starts_with("prefixed") {
                assert_eq!(detect(&v), Some(shape), "{spec}: {v}");
            }
        }
    }

    #[test]
    fn mint_is_not_deterministic() {
        let shape = parse_spec("hex(64)").unwrap();
        assert_ne!(shape.mint().unwrap(), shape.mint().unwrap());
    }

    #[test]
    fn uuid4_mints_valid_v4() {
        let v = Shape::Uuid4.mint().unwrap();
        assert!(is_uuid4(&v), "{v}");
        assert_eq!(detect(&v), Some(Shape::Uuid4));
    }

    #[test]
    fn detection_is_conservative() {
        // a klams-style token: 64 hex chars
        let hex64 = "b7f3a9d2c8e14f60".repeat(4);
        assert_eq!(detect(&hex64), Some(Shape::Hex { chars: 64 }));
        // base64url-charset
        assert_eq!(
            detect("Qx9_zP-aB3cD7eF1Gh24"),
            Some(Shape::Base64Url { chars: 20 })
        );
        // provider tokens are externally issued: no detection, rotate must
        // not pretend it can re-mint one
        assert_eq!(detect("sk-ant-api03-h8Xk2mQv9pLtRw4nZs7cYb1d"), None);
        // free text has no shape
        assert_eq!(detect("postgres"), None);
        assert_eq!(detect("correct horse battery staple"), None);
        // uppercase hex is not our hex (we mint lowercase)
        assert_eq!(
            detect(&"B7F3A9D2C8E14F60".repeat(2)),
            Some(Shape::Base64Url { chars: 32 })
        );
    }

    #[test]
    fn base64url_encoder_is_correct() {
        // RFC 4648 test vectors, URL-safe, unpadded
        assert_eq!(base64url(b""), "");
        assert_eq!(base64url(b"f"), "Zg");
        assert_eq!(base64url(b"fo"), "Zm8");
        assert_eq!(base64url(b"foo"), "Zm9v");
        assert_eq!(base64url(b"foob"), "Zm9vYg");
        assert_eq!(base64url(b"fooba"), "Zm9vYmE");
        assert_eq!(base64url(b"foobar"), "Zm9vYmFy");
        assert_eq!(base64url(&[0xfb, 0xff]), "-_8");
    }
}
