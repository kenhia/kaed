//! Paths kaed refuses to touch, whatever the roots say.
//!
//! Sprint 001 rooted kai at `$HOME`, so `read` on `.config/kaed/env` handed
//! out the very bearer token that gates the service, and `.ssh` was one
//! `edit` away from being rewritten. Narrower roots fix that deployment;
//! this fixes the class.
//!
//! Three layers, deliberately of decreasing absoluteness:
//!
//! 1. **Built-ins** — kaed's own config and journal directories. Not
//!    configurable, so kaed can never serve or rewrite its own credential
//!    no matter how the roots are drawn.
//! 2. **Default globs** — `.ssh`, `.gnupg`, `.env`, `*.pem`, and friends.
//!    On unless `use_default_deny = false`.
//! 3. **Config globs** — `[security] deny`, always additive.
//!
//! A path is denied if it *or any ancestor* matches, so `**/.ssh` covers
//! everything beneath it and operators write the natural pattern. Matching
//! is lexical and absolute: it never touches the filesystem, so it answers
//! identically for paths that exist and paths that don't — a denied path is
//! never an existence oracle.

use globset::{Glob, GlobSet, GlobSetBuilder};
use std::path::{Path, PathBuf};

/// Globs applied unless `use_default_deny = false`. Credentials and key
/// material an agent has no business reading through an editor.
pub const DEFAULT_DENY: &[&str] = &[
    "**/.ssh",
    "**/.gnupg",
    "**/.aws",
    "**/.netrc",
    "**/.git-credentials",
    "**/.config/gh",
    "**/.env",
    "**/.env.*",
    "**/*.pem",
    "**/*.p12",
    "**/id_rsa",
    "**/id_ecdsa",
    "**/id_ed25519",
];

#[derive(Debug)]
pub struct DenyList {
    /// Directory prefixes refused unconditionally, with the label reported.
    builtin: Vec<(PathBuf, &'static str)>,
    set: GlobSet,
    /// Parallel to `set`'s indices, for naming the rule that matched.
    patterns: Vec<String>,
}

impl DenyList {
    /// `builtin` are directories refused unconditionally (kaed's own config
    /// and journal homes), each with the label shown in the error.
    pub fn new(
        builtin: Vec<(PathBuf, &'static str)>,
        globs: &[String],
    ) -> std::result::Result<DenyList, globset::Error> {
        let mut b = GlobSetBuilder::new();
        let mut patterns = Vec::new();
        for g in globs {
            b.add(Glob::new(g)?);
            patterns.push(g.clone());
        }
        Ok(DenyList {
            builtin,
            set: b.build()?,
            patterns,
        })
    }

    /// A deny list with nothing in it — for tests and for roots that have
    /// not been through `Config::resolve`.
    pub fn empty() -> DenyList {
        DenyList::new(Vec::new(), &[]).expect("empty glob set builds")
    }

    /// The rule denying `abs`, or `None` if it is allowed. `abs` must be
    /// absolute; matching walks it and every ancestor.
    pub fn denied_by(&self, abs: &Path) -> Option<String> {
        for (dir, label) in &self.builtin {
            if abs.starts_with(dir) {
                return Some((*label).to_string());
            }
        }
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

    pub fn is_denied(&self, abs: &Path) -> bool {
        self.denied_by(abs).is_some()
    }

    /// Every rule in force, for `check-config` output.
    pub fn describe(&self) -> Vec<String> {
        self.builtin
            .iter()
            .map(|(dir, label)| format!("{} ({})", dir.display(), label))
            .chain(self.patterns.iter().cloned())
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn list(globs: &[&str]) -> DenyList {
        DenyList::new(
            vec![(PathBuf::from("/home/k/.config/kaed"), "kaed's own config")],
            &globs.iter().map(|s| s.to_string()).collect::<Vec<_>>(),
        )
        .unwrap()
    }

    #[test]
    fn builtin_covers_the_directory_and_everything_under_it() {
        let d = list(&[]);
        assert!(d.is_denied(Path::new("/home/k/.config/kaed")));
        assert!(d.is_denied(Path::new("/home/k/.config/kaed/env")));
        assert!(!d.is_denied(Path::new("/home/k/.config/kaedx")));
        assert!(!d.is_denied(Path::new("/home/k/.config/nvim/init.lua")));
        assert_eq!(
            d.denied_by(Path::new("/home/k/.config/kaed/env"))
                .as_deref(),
            Some("kaed's own config")
        );
    }

    #[test]
    fn a_glob_on_a_directory_denies_its_contents() {
        let d = list(&["**/.ssh"]);
        assert!(d.is_denied(Path::new("/home/k/.ssh")));
        assert!(d.is_denied(Path::new("/home/k/.ssh/id_ed25519")));
        assert!(d.is_denied(Path::new("/home/k/.ssh/deep/nested/key")));
        assert!(!d.is_denied(Path::new("/home/k/ssh/notes.md")));
    }

    #[test]
    fn matching_is_lexical_so_missing_and_present_paths_agree() {
        let d = list(&["**/*.pem"]);
        // neither path exists; both are denied on name alone
        assert!(d.is_denied(Path::new("/nowhere/at/all/cert.pem")));
        assert!(d.is_denied(Path::new("/also/missing/cert.pem")));
    }

    #[test]
    fn the_reported_rule_is_the_one_that_matched() {
        let d = list(&["**/.ssh", "**/secrets/**"]);
        assert_eq!(
            d.denied_by(Path::new("/srv/secrets/db.yml")).as_deref(),
            Some("**/secrets/**")
        );
    }

    #[test]
    fn defaults_cover_the_live_test_findings() {
        let d = DenyList::new(
            Vec::new(),
            &DEFAULT_DENY
                .iter()
                .map(|s| s.to_string())
                .collect::<Vec<_>>(),
        )
        .unwrap();
        for p in [
            "/home/ken/.ssh/id_ed25519",
            "/home/ken/.aws/credentials",
            "/home/ken/src/app/.env",
            "/home/ken/src/app/.env.production",
            "/home/ken/certs/server.pem",
        ] {
            assert!(d.is_denied(Path::new(p)), "{p} should be denied");
        }
        assert!(!d.is_denied(Path::new("/home/ken/src/app/src/main.rs")));
        // a file merely *named* like a doc about ssh is fine
        assert!(!d.is_denied(Path::new("/home/ken/notes/ssh-setup.md")));
    }

    #[test]
    fn empty_denies_nothing() {
        assert!(!DenyList::empty().is_denied(Path::new("/home/k/.ssh/id_rsa")));
    }
}
