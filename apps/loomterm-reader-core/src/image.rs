use std::path::{Path, PathBuf};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageReference {
    /// Resolution performs no I/O, existence check, decoding or authorization grant.
    /// The future image loader must revalidate this path at open time.
    Local { path: PathBuf },
    Blocked {
        destination: String,
        reason: BlockedImage,
    },
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BlockedImage {
    NetworkOrScheme,
    InvalidPath,
    MissingDocumentContext,
}

impl ImageReference {
    pub(crate) fn resolve(destination: &str, document: Option<&Path>) -> Self {
        let blocked = |reason| Self::Blocked {
            destination: destination.into(),
            reason,
        };
        // Fragments are not part of the local resource path. Queries are unsupported.
        let raw = destination.split('#').next().unwrap_or_default();
        let Some(decoded) = percent_decode(raw) else {
            return blocked(BlockedImage::InvalidPath);
        };
        if decoded.is_empty() || decoded.contains('?') || decoded.chars().any(char::is_control) {
            return blocked(BlockedImage::InvalidPath);
        }
        if decoded.len() > crate::MAX_PATH_BYTES {
            return blocked(BlockedImage::InvalidPath);
        }
        if !is_local_path(&decoded) {
            return blocked(BlockedImage::NetworkOrScheme);
        }
        let path = Path::new(&decoded);
        if path.is_absolute() {
            return Self::Local { path: path.into() };
        }
        // Reject Windows drive-relative / root-relative / foreign absolute syntax.
        if decoded.contains(':') || decoded.starts_with(['/', '\\']) {
            return blocked(BlockedImage::InvalidPath);
        }
        match document.and_then(Path::parent) {
            Some(parent) if parent.is_absolute() => {
                let path = parent.join(path);
                if path.as_os_str().len() > crate::MAX_PATH_BYTES
                    || !path.to_str().is_some_and(is_local_path)
                {
                    return blocked(BlockedImage::InvalidPath);
                }
                Self::Local { path }
            }
            _ => blocked(BlockedImage::MissingDocumentContext),
        }
    }

    pub(crate) fn payload_bytes(&self) -> usize {
        match self {
            Self::Local { path } => path.capacity(),
            Self::Blocked { destination, .. } => destination.len(),
        }
    }
}

/// Path syntax gate, not a directory ACL or a filesystem sandbox.
pub(crate) fn is_local_path(value: &str) -> bool {
    if value.is_empty() || value.chars().any(char::is_control) {
        return false;
    }
    let portable = value.replace('\\', "/");
    if portable.starts_with("//") {
        return false;
    }
    let bytes = portable.as_bytes();
    let drive = cfg!(windows)
        && bytes.len() > 2
        && bytes[0].is_ascii_alphabetic()
        && bytes[1] == b':'
        && bytes[2] == b'/';
    if portable.contains(':') && (!drive || portable[2..].contains(':')) {
        return false;
    }
    // Windows DOS devices remain special even with an extension; reject cross-platform.
    for component in portable.split('/') {
        let stem = component
            .split('.')
            .next()
            .unwrap_or_default()
            .trim_end_matches(' ')
            .to_ascii_uppercase();
        if matches!(
            stem.as_str(),
            "CON" | "PRN" | "AUX" | "NUL" | "CONIN$" | "CONOUT$"
        ) || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
        {
            return false;
        }
    }
    true
}

fn percent_decode(value: &str) -> Option<String> {
    let mut bytes = Vec::with_capacity(value.len());
    let mut input = value.bytes();
    while let Some(byte) = input.next() {
        bytes.push(if byte == b'%' {
            let hex = |b: u8| (b as char).to_digit(16).map(|v| v as u8);
            hex(input.next()?)? * 16 + hex(input.next()?)?
        } else {
            byte
        });
    }
    String::from_utf8(bytes).ok()
}
