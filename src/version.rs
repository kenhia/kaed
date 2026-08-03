//! What build is this, exactly?
//!
//! `kaed --version` reported `0.1.0` for three sprints running, so the
//! binary on kai and the binary on kubs0 were indistinguishable from the
//! outside — and "is that host running the build with the deny list in it?"
//! had no honest answer short of diffing binaries. Cargo's version tracks
//! sprints loosely at best; the commit is the thing an audit actually wants.
//!
//! [`build.rs`] embeds `git describe --always --dirty` and the commit's
//! date. Neither is available when building from a source tarball with no
//! `.git`, so both degrade to `unknown` rather than failing the build.

/// The crate version alone, e.g. `0.1.0`.
pub const CRATE_VERSION: &str = env!("CARGO_PKG_VERSION");

/// `git describe --always --dirty` at build time, or `unknown`.
pub const GIT_DESCRIBE: &str = env!("KAED_GIT_DESCRIBE");

/// The build commit's author date as `YYYY-MM-DD`, or `unknown`.
pub const COMMIT_DATE: &str = env!("KAED_COMMIT_DATE");

/// The full stamp: `0.1.0 (966367c 2026-08-02)`, or `0.1.0 (unknown)` when
/// built without git.
///
/// This is what `--version` prints and what the MCP handshake reports, so a
/// connected agent and a shell on the host agree about which build is
/// running. A `-dirty` suffix on the hash means the tree had uncommitted
/// changes — the commit date is then the base commit's, not the build's.
///
/// Composed in `build.rs` so it is `&'static str`: clap's `version` takes
/// one, and a runtime `String` would have to be leaked to satisfy it.
pub const FULL: &str = env!("KAED_VERSION_FULL");

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_stamp_names_a_build_and_not_just_a_crate_version() {
        assert!(
            FULL.starts_with(CRATE_VERSION),
            "{FULL} should lead with the version"
        );
        // The whole point of the WI: this must say more than `0.1.0`.
        assert_ne!(FULL, CRATE_VERSION);
        assert!(FULL.contains('('), "{FULL} should carry a build stamp");
    }

    #[test]
    fn a_git_build_reports_a_hash_and_a_date() {
        // The repo's own `cargo test` always has git, so assert the real
        // shape here rather than only the degraded one. A tarball build has
        // no `.git`, hence the escape hatch.
        if GIT_DESCRIBE == "unknown" {
            assert_eq!(COMMIT_DATE, "unknown");
            assert_eq!(FULL, format!("{CRATE_VERSION} (unknown)"));
            return;
        }
        assert!(
            GIT_DESCRIBE.len() >= 7,
            "git describe {GIT_DESCRIBE:?} looks too short to be a hash"
        );
        let date = COMMIT_DATE.as_bytes();
        assert_eq!(
            date.len(),
            10,
            "commit date {COMMIT_DATE:?} should be YYYY-MM-DD"
        );
        assert_eq!(date[4], b'-');
        assert_eq!(date[7], b'-');
        assert!(FULL.contains(GIT_DESCRIBE));
        assert!(FULL.contains(COMMIT_DATE));
    }
}
