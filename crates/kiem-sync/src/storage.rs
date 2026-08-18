//! Filesystem privacy helpers for the on-disk sync data directory.

use std::path::Path;

/// Creates (or repairs) a Kiem data directory before sensitive sync metadata
/// is accessed. Unix permissions are deliberately owner-only; non-Unix uses
/// the platform's native ACL model.
pub(crate) fn ensure_private_data_dir(data_dir: &Path) -> std::io::Result<()> {
    std::fs::create_dir_all(data_dir)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(data_dir, std::fs::Permissions::from_mode(0o700))?;
    }
    Ok(())
}
