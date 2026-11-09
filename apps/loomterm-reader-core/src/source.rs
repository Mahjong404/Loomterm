use crate::{image::is_local_path, Cancellation, Limits, ReaderError, Resource};
use std::{
    fs::{self, Metadata},
    io::Read,
    path::{Path, PathBuf},
    time::SystemTime,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct SourceStamp {
    pub bytes: u64,
    pub modified: Option<SystemTime>,
}

#[derive(Debug)]
pub struct Source {
    pub path: PathBuf,
    pub text: String,
    pub stamp: SourceStamp,
}

fn stamp(metadata: &Metadata) -> SourceStamp {
    SourceStamp {
        bytes: metadata.len(),
        modified: metadata.modified().ok(),
    }
}

fn regular(metadata: &Metadata) -> Result<(), ReaderError> {
    if crate::platform::is_link(metadata) {
        return Err(ReaderError::SymlinkOrReparsePoint);
    }
    if !metadata.is_file() {
        return Err(ReaderError::NotRegularFile);
    }
    Ok(())
}

/// Read an explicitly selected regular local file. UTF-8 (optional BOM) only.
/// Symlink/reparse components are refused, with no directory grant or broker.
/// Checks are best effort against concurrent path replacement, not an OS sandbox.
pub fn read_source(
    path: &Path,
    limits: Limits,
    cancel: &Cancellation,
) -> Result<Source, ReaderError> {
    limits.validate()?;
    let (path, bytes, stamp) = read_local_bytes(path, limits.source_bytes, cancel)?;
    let text = String::from_utf8(bytes).map_err(|e| ReaderError::InvalidEncoding {
        valid_up_to: e.utf8_error().valid_up_to(),
    })?;
    Ok(Source { path, text, stamp })
}

/// Revalidate a local regular file at open time for source or image reads.
/// The byte cap applies to the stream as well as advisory metadata.
pub(crate) fn read_local_bytes(
    path: &Path,
    max_bytes: usize,
    cancel: &Cancellation,
) -> Result<(PathBuf, Vec<u8>, SourceStamp), ReaderError> {
    cancel.check()?;
    if path.as_os_str().len() > crate::MAX_PATH_BYTES {
        return Err(ReaderError::Limit(Resource::PathBytes));
    }
    let display = path.to_str().ok_or(ReaderError::NonLocalPath)?;
    if !is_local_path(display) {
        return Err(ReaderError::NonLocalPath);
    }
    let path = std::path::absolute(path).map_err(|e| ReaderError::io("absolute", e))?;
    if path.as_os_str().len() > crate::MAX_PATH_BYTES {
        return Err(ReaderError::Limit(Resource::PathBytes));
    }
    // Check every component before open, including directory junctions on Windows.
    let mut component_path = PathBuf::new();
    for component in path.components() {
        cancel.check()?;
        component_path.push(component);
        // A bare Windows prefix (C:) denotes drive-relative cwd, not the absolute root.
        if matches!(component, std::path::Component::Prefix(_)) {
            continue;
        }
        let metadata =
            fs::symlink_metadata(&component_path).map_err(|e| ReaderError::io("metadata", e))?;
        if crate::platform::is_link(&metadata) {
            return Err(ReaderError::SymlinkOrReparsePoint);
        }
    }
    let metadata = fs::symlink_metadata(&path).map_err(|e| ReaderError::io("metadata", e))?;
    regular(&metadata)?;
    if metadata.len() > max_bytes as u64 {
        return Err(ReaderError::Limit(Resource::SourceBytes));
    }
    let mut file = crate::platform::open_file(&path).map_err(|e| ReaderError::io("open", e))?;
    let before = file
        .metadata()
        .map_err(|e| ReaderError::io("metadata", e))?;
    regular(&before)?;
    let bytes = read_bounded(&mut file, max_bytes, cancel)?;
    let after = file
        .metadata()
        .map_err(|e| ReaderError::io("metadata", e))?;
    cancel.check()?;
    if stamp(&before) != stamp(&after) || after.len() != bytes.len() as u64 {
        return Err(ReaderError::ChangedDuringRead);
    }
    Ok((path, bytes, stamp(&after)))
}

// Generic Read is only the narrow failure/cancellation seam, not a document platform.
pub(crate) fn read_bounded(
    input: &mut impl Read,
    max: usize,
    cancel: &Cancellation,
) -> Result<Vec<u8>, ReaderError> {
    let mut output = Vec::new();
    let mut buffer = [0; 16 * 1024];
    loop {
        cancel.check()?;
        let count = (max - output.len()).saturating_add(1).min(buffer.len());
        let read = match input.read(&mut buffer[..count]) {
            Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
            Err(e) => return Err(ReaderError::io("read", e)),
            Ok(read) => read,
        };
        cancel.check()?;
        if read == 0 {
            return Ok(output);
        }
        if read > max - output.len() {
            return Err(ReaderError::Limit(Resource::SourceBytes));
        }
        output.extend_from_slice(&buffer[..read]);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failing_and_cancelled_reads_release_input() {
        struct Input {
            token: Cancellation,
            cancel: bool,
            dropped: std::sync::Arc<std::sync::atomic::AtomicBool>,
        }
        impl Read for Input {
            fn read(&mut self, buffer: &mut [u8]) -> std::io::Result<usize> {
                if self.cancel {
                    self.token.cancel();
                    buffer[0] = b'a';
                    Ok(1)
                } else {
                    Err(std::io::ErrorKind::PermissionDenied.into())
                }
            }
        }
        impl Drop for Input {
            fn drop(&mut self) {
                self.dropped
                    .store(true, std::sync::atomic::Ordering::Release);
            }
        }
        for cancel in [false, true] {
            let token = Cancellation::default();
            let dropped = std::sync::Arc::new(std::sync::atomic::AtomicBool::new(false));
            {
                let mut input = Input {
                    token: token.clone(),
                    cancel,
                    dropped: dropped.clone(),
                };
                let result = read_bounded(&mut input, 100, &token);
                if cancel {
                    assert_eq!(result, Err(ReaderError::Cancelled));
                } else {
                    assert_eq!(
                        result,
                        Err(ReaderError::Io {
                            operation: "read",
                            kind: std::io::ErrorKind::PermissionDenied
                        })
                    );
                }
            }
            assert!(dropped.load(std::sync::atomic::Ordering::Acquire));
        }
    }
    #[test]
    fn growth_and_exact_budget() {
        let token = Cancellation::default();
        assert_eq!(
            read_bounded(&mut &b"1234"[..], 3, &token),
            Err(ReaderError::Limit(Resource::SourceBytes))
        );
        assert_eq!(read_bounded(&mut &b"123"[..], 3, &token).unwrap(), b"123");
    }
}
