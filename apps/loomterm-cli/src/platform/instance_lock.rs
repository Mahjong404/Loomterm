//! OS-owned, crash-released file locks for startup arbitration and daemon ownership.
use std::{
    fs::{File, OpenOptions},
    io,
    path::Path,
    time::{Duration, Instant},
};

pub(crate) struct InstanceLock {
    file: File,
}
impl InstanceLock {
    pub(crate) fn acquire(path: &Path, timeout: Duration) -> io::Result<Self> {
        if let Some(parent) = path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let file = OpenOptions::new()
            .create(true)
            .truncate(false)
            .read(true)
            .write(true)
            .open(path)?;
        let end = Instant::now() + timeout;
        loop {
            match lock(&file) {
                Ok(()) => return Ok(Self { file }),
                Err(e) if e.kind() == io::ErrorKind::WouldBlock && Instant::now() < end => {
                    std::thread::sleep(Duration::from_millis(20))
                }
                Err(e) => return Err(e),
            }
        }
    }
}
#[cfg(windows)]
fn lock(file: &File) -> io::Result<()> {
    use std::os::windows::io::AsRawHandle;
    use windows_sys::Win32::{
        Storage::FileSystem::{LockFileEx, LOCKFILE_EXCLUSIVE_LOCK, LOCKFILE_FAIL_IMMEDIATELY},
        System::IO::OVERLAPPED,
    };
    let mut overlapped = OVERLAPPED::default();
    // SAFETY: live owned synchronous file, valid zero-offset output, nonblocking one-byte lock.
    if unsafe {
        LockFileEx(
            file.as_raw_handle(),
            LOCKFILE_EXCLUSIVE_LOCK | LOCKFILE_FAIL_IMMEDIATELY,
            0,
            1,
            0,
            &mut overlapped,
        )
    } != 0
    {
        return Ok(());
    }
    let e = io::Error::last_os_error();
    if matches!(e.raw_os_error(), Some(32 | 33)) {
        Err(io::Error::new(io::ErrorKind::WouldBlock, e))
    } else {
        Err(e)
    }
}
#[cfg(unix)]
fn lock(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;
    // SAFETY: live owned descriptor, advisory exclusive nonblocking lock.
    if unsafe { libc::flock(file.as_raw_fd(), libc::LOCK_EX | libc::LOCK_NB) } == 0 {
        Ok(())
    } else {
        Err(io::Error::last_os_error())
    }
}
impl Drop for InstanceLock {
    fn drop(&mut self) {
        #[cfg(windows)]
        {
            use std::os::windows::io::AsRawHandle;
            let mut o = windows_sys::Win32::System::IO::OVERLAPPED::default();
            // SAFETY: same live file and byte range acquired above; handle close also releases the lock.
            unsafe {
                windows_sys::Win32::Storage::FileSystem::UnlockFileEx(
                    self.file.as_raw_handle(),
                    0,
                    1,
                    0,
                    &mut o,
                )
            };
        }
        #[cfg(unix)]
        {
            use std::os::fd::AsRawFd;
            // SAFETY: live owned descriptor; explicit unlock before close.
            unsafe { libc::flock(self.file.as_raw_fd(), libc::LOCK_UN) };
        }
    }
}
