// Modified for Loomterm (2026-11-05); see apps/loomterm-cli/PATCHES.md.
use std::io;
use std::path::{Path, PathBuf};

/// Optional selector validated against the selected product/session at entry.
pub const CLIENT_SOCKET_PATH_ENV_VAR: &str = "LOOMTERM_CLIENT_SOCKET_PATH";

/// Socket permission mode (owner read/write only).
const SOCKET_PERMISSION_MODE: u32 = 0o600;

/// Always use the selected Loomterm namespace. Arbitrary endpoint overrides
/// are rejected by main; an explicit --session ignores inherited selectors.
pub fn client_socket_path() -> PathBuf {
    crate::session::client_socket_path_for(crate::session::active_name().as_deref())
}

/// Prepares a socket path for binding: creates parent directories,
/// removes stale socket files where no server is listening, and rejects live
/// sockets that are already in use.
pub(crate) fn prepare_socket_path(path: &Path) -> io::Result<()> {
    crate::ipc::prepare_socket_path(path, |path| {
        format!(
            "herdr server is already running (socket busy at {})",
            path.display()
        )
    })
}

/// Restricts socket file permissions to owner-only (0o600).
pub(crate) fn restrict_socket_permissions(path: &Path) -> io::Result<()> {
    crate::ipc::restrict_socket_permissions(path, SOCKET_PERMISSION_MODE)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use std::fs;
    use std::os::unix::net::UnixListener;
    use std::time::Duration;

    #[test]
    fn prepare_socket_path_removes_stale_socket() {
        let dir = PathBuf::from(format!(
            "/tmp/hs-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::create_dir_all(&dir);
        let socket_path = dir.join("stale.sock");

        {
            let _listener = UnixListener::bind(&socket_path).expect("bind stale socket");
        }

        let deadline = std::time::Instant::now() + Duration::from_secs(1);
        while std::time::Instant::now() < deadline {
            if std::os::unix::net::UnixStream::connect(&socket_path).is_err() {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }

        let result = prepare_socket_path(&socket_path);
        assert!(result.is_ok(), "should remove stale socket: {result:?}");
        assert!(!socket_path.exists());

        let _ = fs::remove_dir_all(&dir);
    }

    #[test]
    fn prepare_socket_path_rejects_live_socket() {
        let dir = PathBuf::from(format!(
            "/tmp/hl-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos())
                .unwrap_or(0)
        ));
        let _ = fs::create_dir_all(&dir);
        let socket_path = dir.join("live.sock");

        let _listener = UnixListener::bind(&socket_path).expect("bind");

        let result = prepare_socket_path(&socket_path);
        assert!(result.is_err());
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::AddrInUse);

        let _ = fs::remove_dir_all(&dir);
    }
}
