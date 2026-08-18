use std::fs::{self, File};
use std::io::Write;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};

use iroh::{EndpointId, SecretKey};

/// File inside the data dir holding this device's ed25519 secret key.
pub const IDENTITY_FILE: &str = "identity.key";

/// This device's stable public identity: the `EndpointId` of the persisted
/// key in `data_dir` (created on first use). It addresses the device on the
/// mesh *and* attributes note edits (`author_did`) — one identity for both.
pub fn device_id(data_dir: &Path) -> Result<EndpointId, IdentityError> {
    Ok(load_or_create(&data_dir.join(IDENTITY_FILE))?.public())
}

#[derive(thiserror::Error, Debug)]
pub enum IdentityError {
    #[error("reading identity key at {path}: {source}")]
    Read {
        path: String,
        source: std::io::Error,
    },
    #[error("writing identity key at {path}: {source}")]
    Write {
        path: String,
        source: std::io::Error,
    },
    #[error("securing data directory at {path}: {source}")]
    DataDir {
        path: String,
        source: std::io::Error,
    },
    #[error("securing identity key at {path}: {source}")]
    Permissions {
        path: String,
        source: std::io::Error,
    },
    #[error("stored identity key at {path} is not 32 bytes")]
    Malformed { path: String },
}

/// Loads this device's persisted ed25519 identity, or generates and persists a
/// new one on first run. The same key must be reused across restarts so the
/// device's `EndpointId` (and therefore its address and note authorship) stays
/// stable — never regenerate it as a fallback for a read error.
pub fn load_or_create(key_path: &Path) -> Result<SecretKey, IdentityError> {
    if let Some(parent) = key_path.parent() {
        crate::storage::ensure_private_data_dir(parent).map_err(|source| {
            IdentityError::DataDir {
                path: parent.display().to_string(),
                source,
            }
        })?;
    }
    match fs::read(key_path) {
        Ok(bytes) => {
            restrict_existing_key(key_path).map_err(|source| IdentityError::Permissions {
                path: key_path.display().to_string(),
                source,
            })?;
            decode_key(key_path, bytes)
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            let key = SecretKey::generate();
            if let Some(parent) = key_path.parent() {
                fs::create_dir_all(parent).map_err(|source| IdentityError::Write {
                    path: key_path.display().to_string(),
                    source,
                })?;
            }
            match persist_new_key(key_path, key.to_bytes()) {
                Ok(()) => Ok(key),
                // Another process won the first-create race. Load its key rather
                // than replacing it with ours so a stable existing identity is
                // never rotated.
                Err(source) if source.kind() == std::io::ErrorKind::AlreadyExists => {
                    fs::read(key_path)
                        .map_err(|source| IdentityError::Read {
                            path: key_path.display().to_string(),
                            source,
                        })
                        .and_then(|bytes| decode_key(key_path, bytes))
                }
                Err(source) => Err(IdentityError::Write {
                    path: key_path.display().to_string(),
                    source,
                }),
            }
        }
        Err(source) => Err(IdentityError::Read {
            path: key_path.display().to_string(),
            source,
        }),
    }
}

/// Repairs the permissions of an identity key that existed before the current
/// process opened it. Windows and other non-Unix systems rely on native ACLs.
fn restrict_existing_key(key_path: &Path) -> std::io::Result<()> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::set_permissions(key_path, fs::Permissions::from_mode(0o600))?;
    }
    Ok(())
}

fn decode_key(key_path: &Path, bytes: Vec<u8>) -> Result<SecretKey, IdentityError> {
    let bytes: [u8; 32] = bytes
        .as_slice()
        .try_into()
        .map_err(|_| IdentityError::Malformed {
            path: key_path.display().to_string(),
        })?;
    Ok(SecretKey::from_bytes(&bytes))
}

/// Persists a newly-generated key without ever replacing an identity another
/// process has already created. On Unix the temporary file is created with
/// mode `0600`; non-Unix platforms keep their native ACL semantics. The
/// completed temporary file is linked into place atomically, so readers see
/// either no identity or all 32 bytes — never a default-permission or
/// partially-written key file.
fn persist_new_key(key_path: &Path, key: [u8; 32]) -> std::io::Result<()> {
    let temp_path = unique_temp_path(key_path);
    let mut options = File::options();
    options.write(true).create_new(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }

    let mut file = options.open(&temp_path)?;
    if let Err(error) = file.write_all(&key).and_then(|()| file.sync_all()) {
        drop(file);
        let _ = fs::remove_file(&temp_path);
        return Err(error);
    }
    drop(file);

    match fs::hard_link(&temp_path, key_path) {
        Ok(()) => fs::remove_file(temp_path),
        Err(error) => {
            let _ = fs::remove_file(temp_path);
            Err(error)
        }
    }
}

fn unique_temp_path(key_path: &Path) -> PathBuf {
    static NEXT_TEMP_FILE: AtomicU64 = AtomicU64::new(0);

    let file_name = key_path
        .file_name()
        .and_then(|name| name.to_str())
        .unwrap_or(IDENTITY_FILE);
    key_path.with_file_name(format!(
        ".{file_name}.{}.{}",
        std::process::id(),
        NEXT_TEMP_FILE.fetch_add(1, Ordering::Relaxed)
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn persists_and_reuses_the_same_endpoint_id() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("identity.key");

        let first = load_or_create(&key_path).unwrap();
        let second = load_or_create(&key_path).unwrap();

        assert_eq!(first.public(), second.public());
    }

    #[cfg(unix)]
    #[test]
    fn creates_the_private_key_with_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("identity.key");

        load_or_create(&key_path).unwrap();

        assert_eq!(
            std::fs::metadata(&key_path).unwrap().permissions().mode() & 0o777,
            0o600,
            "a private identity key must not be readable by other users"
        );
    }

    #[cfg(unix)]
    #[test]
    fn creates_and_repairs_its_data_directory_to_owner_only() {
        use std::os::unix::fs::PermissionsExt;

        let root = tempfile::tempdir().unwrap();
        let data_dir = root.path().join("default-data");
        std::fs::create_dir(&data_dir).unwrap();
        fs::set_permissions(&data_dir, fs::Permissions::from_mode(0o755)).unwrap();

        load_or_create(&data_dir.join(IDENTITY_FILE)).unwrap();

        assert_eq!(
            fs::metadata(&data_dir).unwrap().permissions().mode() & 0o777,
            0o700,
            "identity setup must not leave the default data directory exposed"
        );
    }

    #[cfg(unix)]
    #[test]
    fn repairs_an_existing_private_key_to_owner_only_permissions() {
        use std::os::unix::fs::PermissionsExt;

        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("identity.key");
        let first = load_or_create(&key_path).unwrap();
        fs::set_permissions(&key_path, fs::Permissions::from_mode(0o644)).unwrap();

        let repaired = load_or_create(&key_path).unwrap();

        assert_eq!(
            first.public(),
            repaired.public(),
            "repair must not rotate identity"
        );
        assert_eq!(
            fs::metadata(&key_path).unwrap().permissions().mode() & 0o777,
            0o600,
            "an existing identity key must be repaired before it is reused"
        );
    }

    #[test]
    fn rejects_a_malformed_key_file() {
        let dir = tempfile::tempdir().unwrap();
        let key_path = dir.path().join("identity.key");
        std::fs::write(&key_path, b"not a key").unwrap();

        assert!(matches!(
            load_or_create(&key_path),
            Err(IdentityError::Malformed { .. })
        ));
    }
}
