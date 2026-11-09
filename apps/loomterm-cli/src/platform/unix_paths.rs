//! Unix product-directory and filesystem socket safety policy.
use std::{io, path::Path};

pub(crate) fn validate_socket_path(path: &Path, max_bytes: usize) -> io::Result<()> {
    let bytes = path.as_os_str().as_encoded_bytes();
    if bytes.len() > max_bytes || bytes.contains(&0) {
        return Err(io::Error::new(io::ErrorKind::InvalidInput, format!(
            "Unix endpoint needs {} bytes; this platform allows at most {max_bytes}. Use a shorter explicit LOOMTERM_ROOT or XDG runtime base", bytes.len())));
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn validate_native_socket_path(path: &Path) -> io::Result<()> {
    // sun_path includes its terminating NUL: Linux 108 bytes; Darwin 104.
    validate_socket_path(path, if cfg!(target_os = "linux") { 107 } else { 103 })
}

#[cfg(unix)]
pub(crate) fn validate_existing_socket(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{FileTypeExt, MetadataExt};
    let metadata = match std::fs::symlink_metadata(path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(()),
        Err(error) => return Err(error),
    };
    // SAFETY: geteuid reads only the current process identity.
    let uid = unsafe { libc::geteuid() };
    if !metadata.file_type().is_socket() || metadata.uid() != uid {
        return Err(io::Error::new(
            io::ErrorKind::PermissionDenied,
            "refusing to replace a non-socket, symlink or another user's endpoint",
        ));
    }
    Ok(())
}

#[cfg(unix)]
pub(crate) fn create_private_directory(path: &Path) -> io::Result<()> {
    use std::os::unix::fs::{DirBuilderExt, MetadataExt, PermissionsExt};
    fn check(path: &Path) -> io::Result<()> {
        let metadata = std::fs::symlink_metadata(path)?;
        // SAFETY: geteuid has no preconditions; this is the process's own identity.
        let uid = unsafe { libc::geteuid() };
        if !metadata.is_dir() || metadata.file_type().is_symlink() || metadata.uid() != uid {
            return Err(io::Error::new(
                io::ErrorKind::PermissionDenied,
                "product directory must be a real directory owned by the current user",
            ));
        }
        Ok(())
    }
    match std::fs::symlink_metadata(path) {
        Ok(_) => check(path)?,
        Err(error) if error.kind() == io::ErrorKind::NotFound => {
            std::fs::DirBuilder::new()
                .recursive(true)
                .mode(0o700)
                .create(path)?;
            check(path)?;
        }
        Err(error) => return Err(error),
    }
    std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o700))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn m2_socket_length_counts_bytes_and_reserves_the_native_nul() {
        assert!(validate_socket_path(Path::new(&"a".repeat(103)), 103).is_ok());
        assert!(validate_socket_path(Path::new(&"a".repeat(104)), 103).is_err());
        assert!(validate_socket_path(Path::new(&"a".repeat(107)), 107).is_ok());
        assert!(validate_socket_path(Path::new(&"a".repeat(108)), 107).is_err());
        assert!(validate_socket_path(Path::new("中文"), 5).is_err());
    }
    #[cfg(unix)]
    #[test]
    fn m2_private_directory_rejects_symlink_without_changing_target_mode() {
        use std::os::unix::fs::{symlink, PermissionsExt};
        let base = std::env::temp_dir().join(format!("lt-m2-dir-{}", std::process::id()));
        std::fs::create_dir(&base).unwrap();
        let target = base.join("foreign");
        std::fs::create_dir(&target).unwrap();
        std::fs::set_permissions(&target, std::fs::Permissions::from_mode(0o755)).unwrap();
        let link = base.join("product");
        symlink(&target, &link).unwrap();
        assert!(create_private_directory(&link).is_err());
        assert_eq!(
            std::fs::metadata(&target).unwrap().permissions().mode() & 0o777,
            0o755
        );
        std::fs::remove_file(link).unwrap();
        std::fs::remove_dir(target).unwrap();
        std::fs::remove_dir(base).unwrap();
    }
}
