//! Single-mesh-per-data-dir exclusion.
//!
//! Guards the one hazard every `Mesh::start` caller shares: the CLI daemon and
//! the app's FFI bridge both bind a mesh to the same on-disk identity, and
//! running two at once means two accept/dial loops advertising the same
//! `EndpointId`, which corrupts discovery.

use std::fs::{File, TryLockError};
use std::path::Path;

use super::MeshError;

/// Advisory-locked for the life of a running `Mesh`.
const LOCK_FILE: &str = "mesh.lock";

/// Exclusively locks `<data_dir>/mesh.lock` for the caller's lifetime — the
/// returned handle must be kept alive (`Mesh` holds it) for the lock to hold;
/// dropping it releases the lock automatically.
pub(super) fn acquire(data_dir: &Path) -> Result<File, MeshError> {
    // `Mesh::start` may be the very first thing run against a fresh data dir
    // (e.g. `kiem pair add` before any note has ever been created) — this
    // used to be created as a side effect of `identity::load_or_create`,
    // which ran first; now that the lock is acquired first, it must create
    // the dir itself instead of failing on a missing parent.
    crate::storage::ensure_private_data_dir(data_dir).map_err(|source| MeshError::Lock {
        path: data_dir.display().to_string(),
        source,
    })?;
    let path = data_dir.join(LOCK_FILE);
    let mut options = File::options();
    options.create(true).truncate(false).write(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(&path).map_err(|source| MeshError::Lock {
        path: path.display().to_string(),
        source,
    })?;
    match file.try_lock() {
        Ok(()) => Ok(file),
        Err(TryLockError::WouldBlock) => Err(MeshError::AlreadyRunning {
            data_dir: data_dir.display().to_string(),
        }),
        Err(TryLockError::Error(source)) => Err(MeshError::Lock {
            path: path.display().to_string(),
            source,
        }),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn refuses_a_second_holder_until_the_first_is_dropped() {
        let dir = tempfile::tempdir().unwrap();

        let first = acquire(dir.path()).expect("first lock should succeed");
        match acquire(dir.path()) {
            Err(MeshError::AlreadyRunning { .. }) => {}
            other => panic!("expected AlreadyRunning while first lock is held, got {other:?}"),
        }

        drop(first);
        acquire(dir.path()).expect("lock should be free again after the holder drops");
    }

    #[test]
    fn creates_a_data_dir_that_does_not_exist_yet() {
        let root = tempfile::tempdir().unwrap();
        let fresh = root.path().join("never-created").join("nested");

        acquire(&fresh).expect("should create the data dir, not require it to pre-exist");

        assert!(fresh.is_dir());
    }
}
