//! A full-directory backup taken the first time a data dir is opened by a
//! new crate version — the safety net for "don't break existing data, or at
//! least make it easy to roll back" while there's no real migration story
//! yet. Deliberately blunt: a whole-directory copy, not a schema-aware
//! migration framework, since nothing has ever needed a real migration and
//! this covers *any* future format change (SQLite, Automerge, search index)
//! uniformly instead of one column/format at a time.

use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

const MARKER_FILE: &str = ".kiem-data-version";

/// Backs up `data_dir` if it was last opened by a different crate version,
/// then stamps it with the current one. A missing marker (fresh dir, or data
/// from before this existed) is stamped without backing up — there is
/// nothing to roll back to yet.
pub fn check_and_backup(data_dir: &Path) -> std::io::Result<()> {
    let marker = data_dir.join(MARKER_FILE);
    let current = env!("CARGO_PKG_VERSION");

    match std::fs::read_to_string(&marker) {
        Ok(previous) if previous.trim() != current => {
            let backup = backup_path(data_dir, previous.trim());
            copy_dir_all(data_dir, &backup)?;
            eprintln!(
                "kiem: data dir was last opened by v{} (this is v{current}) — backed up to {}",
                previous.trim(),
                backup.display()
            );
            std::fs::write(&marker, current)
        }
        Ok(_) => Ok(()), // unchanged, nothing to do
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => std::fs::write(&marker, current),
        Err(e) => Err(e),
    }
}

fn backup_path(data_dir: &Path, previous_version: &str) -> PathBuf {
    let name = data_dir
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or("kiem-data");
    let epoch_secs = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_secs())
        .unwrap_or(0);
    data_dir.with_file_name(format!("{name}.backup-{previous_version}-{epoch_secs}"))
}

fn copy_dir_all(src: &Path, dst: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(dst)?;
    for entry in std::fs::read_dir(src)? {
        let entry = entry?;
        let dst_path = dst.join(entry.file_name());
        let file_type = entry.file_type()?;
        if file_type.is_dir() {
            copy_dir_all(&entry.path(), &dst_path)?;
        } else if file_type.is_file() {
            std::fs::copy(entry.path(), &dst_path)?;
        } else {
            // The sync daemon's control.sock (or any other special file:
            // FIFO, symlink) holds no data worth backing up, and fs::copy
            // fails on it outright (ENOTSUP on a socket) — skip rather than
            // fail the whole backup over an ephemeral IPC fixture.
            eprintln!(
                "kiem: skipping non-regular file in data-dir backup: {}",
                entry.path().display()
            );
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    /// An isolated parent + data dir, so sibling-directory assertions (the
    /// backup lands next to `data_dir`) can't see another parallel test's
    /// backups in a shared OS temp root.
    fn isolated_data_dir() -> (tempfile::TempDir, PathBuf) {
        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("data");
        std::fs::create_dir_all(&data_dir).unwrap();
        (root, data_dir)
    }

    fn has_backup_entry(root: &Path) -> bool {
        std::fs::read_dir(root)
            .unwrap()
            .filter_map(|e| e.ok())
            .any(|e| e.file_name().to_string_lossy().contains("backup"))
    }

    #[test]
    fn stamps_a_fresh_dir_without_backing_up() {
        let (root, data_dir) = isolated_data_dir();
        check_and_backup(&data_dir).unwrap();

        let marker = std::fs::read_to_string(data_dir.join(MARKER_FILE)).unwrap();
        assert_eq!(marker, env!("CARGO_PKG_VERSION"));
        assert!(!has_backup_entry(root.path()));
    }

    #[test]
    fn backs_up_on_version_change_and_restamps() {
        let (root, data_dir) = isolated_data_dir();
        std::fs::write(data_dir.join(MARKER_FILE), "0.0.1-previous").unwrap();
        std::fs::write(data_dir.join("kiem.db"), b"pretend-database").unwrap();

        check_and_backup(&data_dir).unwrap();

        assert_eq!(
            std::fs::read_to_string(data_dir.join(MARKER_FILE)).unwrap(),
            env!("CARGO_PKG_VERSION")
        );
        let backup = std::fs::read_dir(root.path())
            .unwrap()
            .filter_map(|e| e.ok())
            .find(|e| {
                e.file_name()
                    .to_string_lossy()
                    .contains("backup-0.0.1-previous")
            })
            .expect("a backup dir was created");
        assert_eq!(
            std::fs::read(backup.path().join("kiem.db")).unwrap(),
            b"pretend-database"
        );
    }

    #[test]
    fn does_nothing_when_version_is_unchanged() {
        let (root, data_dir) = isolated_data_dir();
        check_and_backup(&data_dir).unwrap();
        check_and_backup(&data_dir).unwrap(); // second open, same version

        assert!(!has_backup_entry(root.path()));
    }

    /// Regression: a live `kiem sync` daemon leaves a Unix domain socket
    /// (`control.sock`) in the data dir. `fs::copy` can't copy a socket
    /// (ENOTSUP) — this used to take down the whole backup, and therefore
    /// every `NoteStore::open_dir` call, on the first launch of any new
    /// version while the daemon's socket was still on disk.
    #[test]
    fn backing_up_a_data_dir_containing_a_unix_socket_does_not_fail() {
        let (_root, data_dir) = isolated_data_dir();
        std::fs::write(data_dir.join(MARKER_FILE), "0.0.1-previous").unwrap();
        std::fs::write(data_dir.join("kiem.db"), b"pretend-database").unwrap();
        let _listener =
            std::os::unix::net::UnixListener::bind(data_dir.join("control.sock")).unwrap();

        check_and_backup(&data_dir).unwrap();

        assert_eq!(
            std::fs::read_to_string(data_dir.join(MARKER_FILE)).unwrap(),
            env!("CARGO_PKG_VERSION")
        );
    }
}
