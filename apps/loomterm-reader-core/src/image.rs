use crate::{source::read_local_bytes, Cancellation, ReaderError, Resource};
use image::{error::LimitErrorKind, ImageFormat, ImageReader};
use std::{
    io::Cursor,
    path::{Path, PathBuf},
};

/// Per-decode limits. A host also needs an aggregate, per-window image cache cap.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct ImageLimits {
    pub encoded_bytes: usize,
    pub pixels: u64,
    pub dimension: u32,
}

impl Default for ImageLimits {
    fn default() -> Self {
        Self {
            encoded_bytes: 8 * 1024 * 1024,
            pixels: 4_000_000,
            dimension: 8192,
        }
    }
}

impl ImageLimits {
    fn validate(self) -> Result<Self, ImageLoadError> {
        if self.encoded_bytes > 16 * 1024 * 1024
            || self.pixels > 8_000_000
            || self.dimension > 16_384
        {
            return Err(ImageLoadError::InvalidLimits);
        }
        Ok(self)
    }
}

/// Tightly packed, top-down RGBA8. The host owns this buffer after return and
/// must release it when its ResourceTicket is stale or the Reader HWND closes.
#[derive(Debug)]
pub struct DecodedImage {
    pub width: u32,
    pub height: u32,
    pub rgba: Vec<u8>,
}

impl DecodedImage {
    pub fn stride_bytes(&self) -> usize {
        self.width as usize * 4
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageLoadError {
    Blocked(BlockedImage),
    Source(ReaderError),
    InvalidLimits,
    EncodedBytes,
    Dimensions,
    DecoderBudget,
    UnsupportedFormat,
    Decode,
}

impl std::fmt::Display for ImageLoadError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ImageLoadError {}

/// Decode only an explicitly resolved local image. No URL loader, HTML or script.
/// Blocking I/O and codec work belong on a bounded worker, never the UI thread.
/// Cancellation is cooperative at read and before/after decode, not inside codecs.
pub fn load_local_image(
    reference: &ImageReference,
    limits: ImageLimits,
    cancel: &Cancellation,
) -> Result<DecodedImage, ImageLoadError> {
    let limits = limits.validate()?;
    let path = match reference {
        ImageReference::Local { path } => path,
        ImageReference::Blocked { reason, .. } => return Err(ImageLoadError::Blocked(*reason)),
    };
    let (_, bytes, _) = read_local_bytes(path, limits.encoded_bytes, cancel).map_err(|error| {
        if error == ReaderError::Limit(Resource::SourceBytes) {
            ImageLoadError::EncodedBytes
        } else {
            ImageLoadError::Source(error)
        }
    })?;
    cancel.check().map_err(ImageLoadError::Source)?;
    let format = image::guess_format(&bytes).map_err(|_| ImageLoadError::UnsupportedFormat)?;
    if !matches!(
        format,
        ImageFormat::Png | ImageFormat::Jpeg | ImageFormat::WebP
    ) {
        return Err(ImageLoadError::UnsupportedFormat);
    }
    let reader = || {
        let mut reader = ImageReader::with_format(Cursor::new(bytes.as_slice()), format);
        let mut decode_limits = image::Limits::default();
        decode_limits.max_image_width = Some(limits.dimension);
        decode_limits.max_image_height = Some(limits.dimension);
        // Decoder intermediate allocations are best effort in image 0.25;
        // the output size is checked strictly below before conversion.
        decode_limits.max_alloc = Some(limits.pixels.saturating_mul(16));
        reader.limits(decode_limits);
        reader
    };
    let (width, height) = reader().into_dimensions().map_err(map_decode_error)?;
    let pixels = u64::from(width) * u64::from(height);
    if width == 0
        || height == 0
        || width > limits.dimension
        || height > limits.dimension
        || pixels > limits.pixels
        || pixels
            .checked_mul(4)
            .and_then(|n| usize::try_from(n).ok())
            .is_none()
    {
        return Err(ImageLoadError::Dimensions);
    }
    cancel.check().map_err(ImageLoadError::Source)?;
    let decoded = reader().decode().map_err(map_decode_error)?;
    cancel.check().map_err(ImageLoadError::Source)?;
    let rgba = decoded.into_rgba8();
    if rgba.dimensions() != (width, height) || rgba.as_raw().len() != (pixels * 4) as usize {
        return Err(ImageLoadError::Decode);
    }
    Ok(DecodedImage {
        width,
        height,
        rgba: rgba.into_raw(),
    })
}

fn map_decode_error(error: image::ImageError) -> ImageLoadError {
    match error {
        image::ImageError::Limits(limit) => match limit.kind() {
            LimitErrorKind::DimensionError => ImageLoadError::Dimensions,
            _ => ImageLoadError::DecoderBudget,
        },
        _ => ImageLoadError::Decode,
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ImageReference {
    /// Resolution performs no I/O, existence check, decoding or authorization grant.
    /// The explicit image loader revalidates this path at open time.
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
