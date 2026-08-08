//! The OS as a named policy layer (sprint 014, korg #1088/#1091/#1092).
//!
//! kaed had four policy layers with vocabulary — the deny list,
//! `.kaedignore`, the in-file marker, classification — and a fifth with
//! none: unix ownership. A root-owned file produced a bare
//! `internal: … Permission denied (os error 13)`, the one error shape in
//! the contract carrying no recovery data, which is precisely what the
//! structured-error posture exists to prevent.
//!
//! Three rules govern everything here:
//!
//! - **It is `denied`, with its own `reason` (D-1).** Not a new error code:
//!   `denied` already means "this will not work, and the remedy is not a
//!   different path", which is true here. What was missing was the
//!   `reason`, and `reason` is the field whose whole job is naming which
//!   layer refused.
//! - **Writability is a question about the DIRECTORY (D-2).** kaed writes
//!   atomically: stage a temp file beside the destination, then rename over
//!   it. `rename(2)` needs write+execute on the containing directory and
//!   cares nothing for the destination file's own mode — so a root-owned
//!   `0644` file inside a writable directory *is* writable through kaed,
//!   and asking `access(file, W_OK)` would have refused a write that works.
//! - **The route to the editable copy is config, not code (D-3).** Where a
//!   host's rendered artifacts are managed from is a fact about that host.
//!   The refusal carries the addressed root's own `description` as
//!   `root_advisory`, so the advisory an operator writes once in
//!   `config.toml` is the same text that arrives at the point of failure.

use crate::config::ResolvedRoot;
use crate::errors::{KaedError, RefusalReason};
use serde_json::json;
use std::os::unix::fs::MetadataExt;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;

/// Did the OS refuse this on permissions? `PermissionDenied` covers both
/// `EACCES` and `EPERM`, which is the granularity that matters: both mean
/// "this identity may not", and neither is a path to correct.
pub fn is_permission_denied(e: &std::io::Error) -> bool {
    e.kind() == std::io::ErrorKind::PermissionDenied
}

/// Whether kaed could stage-and-rename into this directory. The one
/// question the write path actually asks (D-2).
///
/// `EACCESS` so the check uses the *effective* ids — the same ones the
/// write itself will be checked against. A directory that does not exist
/// answers `true`: creating it is the write path's job, and its own
/// failure will land here with a real answer.
pub fn dir_is_writable(dir: &Path) -> bool {
    use rustix::fs::{Access, AtFlags};
    match rustix::fs::accessat(
        rustix::fs::CWD,
        dir,
        Access::WRITE_OK | Access::EXEC_OK,
        AtFlags::EACCESS,
    ) {
        Ok(()) => true,
        Err(e) => e != rustix::io::Errno::ACCESS && e != rustix::io::Errno::PERM,
    }
}

/// Who kaed is, to the kernel. The uid is the fact; the name is a
/// convenience read from the environment systemd's `User=` sets, and is
/// omitted rather than guessed when it is absent.
fn service_identity() -> serde_json::Value {
    let uid = rustix::process::geteuid().as_raw();
    match std::env::var("USER").or_else(|_| std::env::var("LOGNAME")) {
        Ok(user) if !user.is_empty() => json!({ "uid": uid, "user": user }),
        _ => json!({ "uid": uid }),
    }
}

/// The owning uid/gid and mode of a path, when it can still be stat'ed.
/// Absent is a fine answer — the refusal is already true without it.
fn owner_of(p: &Path) -> Option<serde_json::Value> {
    let meta = std::fs::symlink_metadata(p).ok()?;
    Some(json!({
        "uid": meta.uid(),
        "gid": meta.gid(),
        "mode": format!("{:04o}", meta.permissions().mode() & 0o7777),
    }))
}

/// The evidence block every permission refusal carries: who kaed is, what
/// owns the thing that refused, and whatever this root's `description`
/// says about where its files are managed from (D-3).
fn evidence(root: &ResolvedRoot, subject: &Path, subject_label: &str) -> serde_json::Value {
    let mut data = json!({
        "service_identity": service_identity(),
        subject_label: subject
            .strip_prefix(&root.path)
            .map(|r| r.to_string_lossy().into_owned())
            .unwrap_or_else(|_| subject.display().to_string()),
    });
    let obj = data.as_object_mut().expect("evidence is an object");
    if let Some(owner) = owner_of(subject) {
        obj.insert("owner".into(), owner);
    }
    if let Some(d) = &root.description {
        obj.insert("root_advisory".into(), json!(d));
    }
    data
}

/// What to append to a hint when the root has something to say about
/// itself. This is the whole of D-3: no host's directory layout is named
/// in kaed's source.
fn advisory(root: &ResolvedRoot) -> String {
    match &root.description {
        Some(d) => format!(
            " This root describes itself as: {d:?} — if that names where these files are \
             managed from, edit them there."
        ),
        None => String::new(),
    }
}

/// The OS refused a read of `rel`. Distinct from every other `denied`: the
/// file is genuinely there and `list` can still see its name, so an agent
/// that just listed the directory needs to be told what changed between
/// seeing the name and being refused the bytes.
pub fn not_readable(root: &ResolvedRoot, rel: &str, abs: &Path) -> KaedError {
    KaedError::refused(
        rel,
        "unix permissions",
        RefusalReason::NotReadableByServiceIdentity,
        format!(
            "the identity kaed runs as cannot open this file — a fact about the host, \
             not a kaed policy: no deny rule, `.kaedignore` or classification is \
             involved, which is why `list` can still see the name. Retrying will not \
             help and no other path under this root will either.{}",
            advisory(root)
        ),
    )
    .merge_data(evidence(root, abs, "path"))
}

/// The OS will refuse a write to `rel`. `dir` is the **directory that was
/// probed**, passed in rather than derived, because for a create into a
/// not-yet-existing subtree the relevant directory is the deepest existing
/// ancestor and not the immediate parent (D-2). Reporting the destination
/// file's own mode here would be true and useless.
pub fn not_writable(root: &ResolvedRoot, rel: &str, dir: &Path) -> KaedError {
    KaedError::refused(
        rel,
        "unix permissions",
        RefusalReason::NotWritableByServiceIdentity,
        format!(
            "kaed writes atomically — a temp file beside the destination, then a rename \
             over it — so a write needs write+execute on the CONTAINING DIRECTORY, and \
             the identity kaed runs as does not have it. The file's own mode is not the \
             obstacle. A fact about the host, not a kaed policy; reading it may well \
             still work.{}",
            advisory(root)
        ),
    )
    .merge_data(evidence(root, dir, "directory"))
    .merge_data(json!({ "path": rel }))
}

/// Root bypasses DAC, so every test in this sprint is vacuous under it.
/// Skipping beats a test that passes for the wrong reason.
#[cfg(test)]
pub(crate) fn running_as_root() -> bool {
    rustix::process::geteuid().is_root()
}

/// Map an IO failure on the read path, keeping the permission case
/// structured and letting everything else fall through to the mapping it
/// already had.
pub fn map_read_io(root: &ResolvedRoot, rel: &str, abs: &Path, e: std::io::Error) -> KaedError {
    if is_permission_denied(&e) {
        not_readable(root, rel, abs)
    } else {
        KaedError::from(e)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::errors::ErrorCode;

    fn root(dir: &Path) -> ResolvedRoot {
        ResolvedRoot::unrestricted("t", dir.canonicalize().unwrap())
    }

    /// D-2, stated as a test because the naive check gets it backwards: a
    /// file kaed cannot write *as a file* is writable through kaed when its
    /// directory allows the rename.
    #[test]
    fn writability_is_a_property_of_the_directory_not_the_file() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("readonly.txt");
        std::fs::write(&f, "x").unwrap();
        std::fs::set_permissions(&f, std::fs::Permissions::from_mode(0o444)).unwrap();

        assert!(
            dir_is_writable(f.parent().unwrap()),
            "the directory allows the rename, so kaed can write this file"
        );
        // …and it really can: the atomic path never opens the file itself.
        let staged = crate::fsops::stage(&f, b"y", 0o644).unwrap();
        crate::fsops::promote(&staged).unwrap();
        assert_eq!(std::fs::read_to_string(&f).unwrap(), "y");
    }

    #[test]
    fn an_unwritable_directory_is_recognized_as_one() {
        if running_as_root() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        let sub = dir.path().join("locked");
        std::fs::create_dir(&sub).unwrap();
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o555)).unwrap();
        assert!(!dir_is_writable(&sub));
        // restore so the tempdir can clean itself up
        std::fs::set_permissions(&sub, std::fs::Permissions::from_mode(0o755)).unwrap();
    }

    /// The whole point of the sprint: what an agent gets back has a path, a
    /// named reason, and a hint — the three things os-error-13 had none of.
    #[test]
    fn a_permission_refusal_carries_path_reason_and_evidence() {
        let dir = tempfile::tempdir().unwrap();
        let f = dir.path().join("secret.conf");
        std::fs::write(&f, "x").unwrap();
        let err = not_readable(&root(dir.path()), "secret.conf", &f);
        let v = serde_json::to_value(&err).unwrap();
        assert_eq!(v["code"], "denied");
        assert_eq!(v["data"]["reason"], "not_readable_by_service_identity");
        assert_eq!(v["data"]["path"], "secret.conf");
        assert!(v["data"]["hint"].is_string());
        assert!(v["data"]["service_identity"]["uid"].is_u64());
        assert!(v["data"]["owner"]["mode"].is_string());
        assert_eq!(err.code, ErrorCode::Denied);
    }

    /// D-3: the route to the editable copy is whatever the operator wrote
    /// in `config.toml`, verbatim — kaed's source names no host's layout.
    #[test]
    fn the_roots_own_description_is_the_advisory() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("prometheus.yml"), "x").unwrap();
        let root = ResolvedRoot {
            description: Some(
                "MANAGED: rendered by k-homelab's docker-services recipe; edit the source at \
                 kubs0:k-homelab/recipes/docker-services/files/<svc>/"
                    .into(),
            ),
            ..root(dir.path())
        };
        let err = not_writable(&root, "prometheus.yml", &root.path.clone());
        let v = serde_json::to_value(&err).unwrap();
        assert_eq!(v["data"]["reason"], "not_writable_by_service_identity");
        assert_eq!(v["data"]["path"], "prometheus.yml");
        let advisory = v["data"]["root_advisory"].as_str().unwrap();
        assert!(advisory.contains("docker-services"), "{advisory}");
        assert!(
            err.message.contains("docker-services"),
            "the advisory reaches the message too: {}",
            err.message
        );
        // The evidence is about the directory, not the file (D-2).
        assert_eq!(v["data"]["directory"], "");
    }

    /// A permission failure is kaed's problem to explain, so it keeps the
    /// friction invitation `denied` already carried.
    #[test]
    fn a_permission_refusal_still_invites_feedback() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("f"), "x").unwrap();
        let err =
            not_readable(&root(dir.path()), "f", &dir.path().join("f")).with_feedback_invite();
        let v = serde_json::to_value(&err).unwrap();
        assert!(v["data"]["feedback_invite"]["ask"].is_string());
        assert_eq!(v["data"]["reason"], "not_readable_by_service_identity");
    }

    #[test]
    fn non_permission_io_errors_keep_their_old_mapping() {
        let dir = tempfile::tempdir().unwrap();
        let e = std::io::Error::new(std::io::ErrorKind::NotFound, "gone");
        let mapped = map_read_io(&root(dir.path()), "f", &dir.path().join("f"), e);
        assert_eq!(mapped.code, ErrorCode::NotFound);
    }
}
