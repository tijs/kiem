use std::path::Path;

use iroh::SecretKey;

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
    #[error("stored identity key at {path} is not 32 bytes")]
    Malformed { path: String },
}

/// Loads this device's persisted ed25519 identity, or generates and persists a
/// new one on first run. The same key must be reused across restarts so the
/// device's `EndpointId` (and therefore its address and note authorship) stays
/// stable — never regenerate it as a fallback for a read error.
pub fn load_or_create(key_path: &Path) -> Result<SecretKey, IdentityError> {
    match std::fs::read(key_path) {
        Ok(bytes) => {
            let bytes: [u8; 32] =
                bytes
                    .as_slice()
                    .try_into()
                    .map_err(|_| IdentityError::Malformed {
                        path: key_path.display().to_string(),
                    })?;
            Ok(SecretKey::from_bytes(&bytes))
        }
        Err(source) if source.kind() == std::io::ErrorKind::NotFound => {
            let key = SecretKey::generate();
            if let Some(parent) = key_path.parent() {
                std::fs::create_dir_all(parent).map_err(|source| IdentityError::Write {
                    path: key_path.display().to_string(),
                    source,
                })?;
            }
            std::fs::write(key_path, key.to_bytes()).map_err(|source| IdentityError::Write {
                path: key_path.display().to_string(),
                source,
            })?;
            Ok(key)
        }
        Err(source) => Err(IdentityError::Read {
            path: key_path.display().to_string(),
            source,
        }),
    }
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
