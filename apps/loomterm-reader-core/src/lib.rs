//! Read-only, local-file Markdown content. No UI, terminal, network or runtime dependency.
//! Run requests on a bounded worker; parsing and filesystem calls are synchronous.
//!
//! ```no_run
//! use loomterm_reader_core::{Apply, Limits, Reader};
//! use std::path::Path;
//! # fn main() -> Result<(), loomterm_reader_core::ReaderError> {
//! let mut reader = Reader::new(Limits::default())?;
//! let request = reader.open(Path::new("selected.md"))?;
//! // Move execute() to a bounded worker in a GUI host.
//! let result = request.execute();
//! if let Apply::Ready(_) = reader.apply(result) {
//!     let content = &reader.document().unwrap().content;
//!     for (_, node) in content.children(0) { println!("{:?}", node.kind); }
//! }
//! # Ok(())
//! # }
//! ```
#![forbid(unsafe_code)]

mod image;
mod model;
mod parse;
mod platform;
mod reader;
mod source;
pub mod view;

pub use image::{BlockedImage, ImageReference};
pub use model::*;
pub use parse::parse_markdown;
pub use reader::{Apply, Document, LoadRequest, LoadResult, Reader, ReaderState, Update};
pub use source::{read_source, Source, SourceStamp};

use std::sync::{
    atomic::{AtomicBool, Ordering},
    Arc,
};

/// Shared cooperative cancellation. Tokens are per request, never reset or reused.
#[derive(Clone, Debug, Default)]
pub struct Cancellation(Arc<AtomicBool>);

impl Cancellation {
    pub(crate) fn same_request(&self, other: &Self) -> bool {
        Arc::ptr_eq(&self.0, &other.0)
    }
    pub fn cancel(&self) {
        self.0.store(true, Ordering::Release);
    }
    pub fn is_cancelled(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
    pub(crate) fn check(&self) -> Result<(), ReaderError> {
        if self.is_cancelled() {
            Err(ReaderError::Cancelled)
        } else {
            Ok(())
        }
    }
}

/// Lowerable budgets; hard ceilings prevent accidental unbounded configurations.
#[derive(Clone, Copy, Debug)]
pub struct Limits {
    pub source_bytes: usize,
    pub nodes: usize,
    pub depth: usize,
    pub image_references: usize,
    pub payload_bytes: usize,
}

impl Default for Limits {
    fn default() -> Self {
        Self {
            source_bytes: 4 * 1024 * 1024,
            nodes: 100_000,
            depth: 64,
            image_references: 256,
            payload_bytes: 8 * 1024 * 1024,
        }
    }
}

impl Limits {
    pub(crate) fn validate(&self) -> Result<(), ReaderError> {
        if self.source_bytes > 16 * 1024 * 1024
            || self.nodes > 250_000
            || self.depth > 128
            || self.image_references > 4096
            || self.payload_bytes > 32 * 1024 * 1024
        {
            return Err(ReaderError::InvalidLimits);
        }
        Ok(())
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Resource {
    SourceBytes,
    Nodes,
    Depth,
    ImageReferences,
    PayloadBytes,
    PathBytes,
}

/// Encoded path bytes, independently capped before duplicating document-relative references.
pub const MAX_PATH_BYTES: usize = 32 * 1024;

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReaderError {
    Io {
        operation: &'static str,
        kind: std::io::ErrorKind,
    },
    InvalidEncoding {
        valid_up_to: usize,
    },
    Limit(Resource),
    InvalidLimits,
    NonLocalPath,
    NotRegularFile,
    SymlinkOrReparsePoint,
    ChangedDuringRead,
    Cancelled,
    UnsupportedSyntax,
    GenerationExhausted,
}

impl ReaderError {
    pub(crate) fn io(operation: &'static str, error: std::io::Error) -> Self {
        Self::Io {
            operation,
            kind: error.kind(),
        }
    }
}
impl std::fmt::Display for ReaderError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "{self:?}")
    }
}
impl std::error::Error for ReaderError {}
