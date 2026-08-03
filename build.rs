//! Embed the git commit and its date so `kaed --version` can tell one build
//! from another (korg #924).
//!
//! Both values degrade to `unknown` when git is unavailable or the source
//! is not a checkout — building kaed from a tarball must work, and a build
//! script that fails the build to report a missing hash would be a poor
//! trade for a cosmetic string.

use std::path::Path;
use std::process::Command;

fn main() {
    // Rerun when HEAD moves or the index changes; otherwise cargo caches the
    // stamp from whichever commit happened to be checked out first, which is
    // exactly the staleness this is meant to cure.
    for p in [".git/HEAD", ".git/index"] {
        if Path::new(p).exists() {
            println!("cargo:rerun-if-changed={p}");
        }
    }
    // `.git/HEAD` on a branch points at a ref whose file moves on commit.
    if let Some(head) = read_git("rev-parse --symbolic-full-name HEAD") {
        let refpath = format!(".git/{head}");
        if Path::new(&refpath).exists() {
            println!("cargo:rerun-if-changed={refpath}");
        }
    }

    let describe = read_git("describe --always --dirty").unwrap_or_else(|| "unknown".into());
    let date = read_git("log -1 --format=%cd --date=short").unwrap_or_else(|| "unknown".into());
    let crate_version = std::env::var("CARGO_PKG_VERSION").expect("cargo sets this");

    // Composed here rather than in the crate so it can be a `&'static str`:
    // clap's `version` takes one, and a `String` would have to be leaked.
    let full = if describe == "unknown" {
        format!("{crate_version} (unknown)")
    } else {
        format!("{crate_version} ({describe} {date})")
    };

    println!("cargo:rustc-env=KAED_GIT_DESCRIBE={describe}");
    println!("cargo:rustc-env=KAED_COMMIT_DATE={date}");
    println!("cargo:rustc-env=KAED_VERSION_FULL={full}");
}

/// Run `git <args>` and return trimmed stdout, or `None` if git is missing,
/// this is not a repository, or the command failed for any other reason.
fn read_git(args: &str) -> Option<String> {
    let out = Command::new("git")
        .args(args.split_whitespace())
        .output()
        .ok()?;
    if !out.status.success() {
        return None;
    }
    let s = String::from_utf8(out.stdout).ok()?.trim().to_string();
    if s.is_empty() { None } else { Some(s) }
}
