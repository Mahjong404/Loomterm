use crate::{parse_markdown, read_source, Cancellation, Content, Limits, ReaderError, SourceStamp};
use std::{
    ops::Range,
    path::{Path, PathBuf},
    sync::Arc,
};

#[derive(Debug)]
pub struct Document {
    pub path: PathBuf,
    pub stamp: SourceStamp,
    pub content: Arc<Content>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReaderState {
    Empty,
    Loading { generation: u64 },
    Ready { generation: u64 },
    Cancelled { generation: u64 },
    Failed { generation: u64, error: ReaderError },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Update {
    Unchanged,
    /// UTF-8 boundaries for presentation invalidation, NOT an independently parsable fragment.
    /// Markdown reference definitions may change nodes outside this range: AST replacement is atomic.
    Replaced {
        old: Range<usize>,
        new: Range<usize>,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum Apply {
    Stale,
    Ready(Update),
    Cancelled,
    Failed(ReaderError),
}

/// One private content slot per client. No global registry, background watcher or task queue.
/// The host owns window cardinality, a bounded worker, polling/debounce and scroll position.
pub struct Reader {
    limits: Limits,
    generation: u64,
    path: Option<PathBuf>,
    active: Option<Cancellation>,
    current: Option<Arc<Document>>,
    state: ReaderState,
    following: bool,
}

impl Reader {
    pub fn new(limits: Limits) -> Result<Self, ReaderError> {
        limits.validate()?;
        Ok(Self {
            limits,
            generation: 0,
            path: None,
            active: None,
            current: None,
            state: ReaderState::Empty,
            following: true,
        })
    }
    pub fn state(&self) -> &ReaderState {
        &self.state
    }
    /// On reload failure/cancellation, this is the last good revision for the SAME target.
    pub fn document(&self) -> Option<&Arc<Document>> {
        self.current.as_ref()
    }
    pub fn following(&self) -> bool {
        self.following
    }
    pub fn set_following(&mut self, follow: bool) {
        self.following = follow;
    }

    /// Explicit file selection. Replaces this client's target and cancels its older task.
    pub fn open(&mut self, path: &Path) -> Result<LoadRequest, ReaderError> {
        if path.as_os_str().len() > crate::MAX_PATH_BYTES {
            return Err(ReaderError::Limit(crate::Resource::PathBytes));
        }
        if !path.to_str().is_some_and(crate::image::is_local_path) {
            return Err(ReaderError::NonLocalPath);
        }
        let path = std::path::absolute(path).map_err(|e| ReaderError::io("absolute", e))?;
        if path.as_os_str().len() > crate::MAX_PATH_BYTES {
            return Err(ReaderError::Limit(crate::Resource::PathBytes));
        }
        let generation = self
            .generation
            .checked_add(1)
            .ok_or(ReaderError::GenerationExhausted)?;
        if let Some(token) = self.active.take() {
            token.cancel();
        }
        if self.path.as_ref() != Some(&path) {
            self.current = None;
        }
        self.path = Some(path.clone());
        self.generation = generation;
        let cancel = Cancellation::default();
        self.active = Some(cancel.clone());
        self.state = ReaderState::Loading { generation };
        Ok(LoadRequest {
            generation,
            path,
            limits: self.limits,
            cancel,
            previous: self.current.clone(),
        })
    }

    /// Manual refresh always works while paused; no target means no work.
    pub fn reload(&mut self) -> Result<Option<LoadRequest>, ReaderError> {
        match self.path.clone() {
            Some(path) => self.open(&path).map(Some),
            None => Ok(None),
        }
    }

    /// Call from a host's debounced file-change/poll scheduler. Busy requests coalesce.
    /// A bounded full read detects same-length/same-timestamp saves too.
    pub fn poll(&mut self) -> Result<Option<LoadRequest>, ReaderError> {
        if !self.following || self.active.is_some() {
            return Ok(None);
        }
        self.reload()
    }

    pub fn cancel(&mut self) {
        if let Some(token) = self.active.take() {
            token.cancel();
            self.state = ReaderState::Cancelled {
                generation: self.generation,
            };
        }
    }

    pub fn clear(&mut self) {
        self.cancel();
        self.current = None;
        self.path = None;
        self.state = ReaderState::Empty;
    }

    /// Installs without parsing/diffing; old generations cannot replace a newer target.
    /// Retain the old document before applying and drop that Arc on a worker to defer deallocation.
    pub fn apply(&mut self, result: LoadResult) -> Apply {
        if result.generation != self.generation || self.path.as_ref() != Some(&result.path) {
            return Apply::Stale;
        }
        if !self
            .active
            .as_ref()
            .is_some_and(|active| active.same_request(&result.cancel))
        {
            return Apply::Stale;
        }
        self.active = None;
        if result.cancel.is_cancelled() {
            self.state = ReaderState::Cancelled {
                generation: self.generation,
            };
            return Apply::Cancelled;
        }
        match result.outcome {
            Ok((document, update)) => {
                self.current = Some(document);
                self.state = ReaderState::Ready {
                    generation: self.generation,
                };
                Apply::Ready(update)
            }
            Err(ReaderError::Cancelled) => {
                self.state = ReaderState::Cancelled {
                    generation: self.generation,
                };
                Apply::Cancelled
            }
            Err(error) => {
                self.state = ReaderState::Failed {
                    generation: self.generation,
                    error: error.clone(),
                };
                Apply::Failed(error)
            }
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        self.cancel();
    }
}

/// Move to a bounded worker. Dropping Reader/starting another request cancels its token.
pub struct LoadRequest {
    generation: u64,
    path: PathBuf,
    limits: Limits,
    cancel: Cancellation,
    previous: Option<Arc<Document>>,
}
impl LoadRequest {
    pub fn cancellation(&self) -> Cancellation {
        self.cancel.clone()
    }
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Synchronous I/O + parse + bounded diff; never call in the GUI draw/event loop.
    pub fn execute(self) -> LoadResult {
        let outcome = self.load();
        LoadResult {
            generation: self.generation,
            path: self.path,
            cancel: self.cancel,
            outcome,
        }
    }
    fn load(&self) -> Result<(Arc<Document>, Update), ReaderError> {
        let source = read_source(&self.path, self.limits, &self.cancel)?;
        let update = match self.previous.as_ref() {
            Some(previous) => changed_range(&previous.content.source, &source.text, &self.cancel)?,
            None => Update::Replaced {
                old: 0..0,
                new: 0..source.text.len(),
            },
        };
        let content = if update == Update::Unchanged {
            self.previous
                .as_ref()
                .expect("unchanged requires previous content")
                .content
                .clone()
        } else {
            Arc::new(parse_markdown(
                source.text,
                Some(&source.path),
                self.limits,
                &self.cancel,
            )?)
        };
        self.cancel.check()?;
        Ok((
            Arc::new(Document {
                path: source.path,
                stamp: source.stamp,
                content,
            }),
            update,
        ))
    }
}

pub struct LoadResult {
    generation: u64,
    path: PathBuf,
    cancel: Cancellation,
    outcome: Result<(Arc<Document>, Update), ReaderError>,
}

impl LoadResult {
    /// Borrow a successful worker result to prepare renderer-independent view data off-thread.
    pub fn document(&self) -> Option<&Arc<Document>> {
        self.outcome.as_ref().ok().map(|(document, _)| document)
    }
    pub fn update(&self) -> Option<&Update> {
        self.outcome.as_ref().ok().map(|(_, update)| update)
    }
}

fn changed_range(old: &str, new: &str, cancel: &Cancellation) -> Result<Update, ReaderError> {
    let mut prefix = 0;
    for (index, (a, b)) in old.bytes().zip(new.bytes()).enumerate() {
        if index % 16384 == 0 {
            cancel.check()?;
        }
        if a != b {
            break;
        }
        prefix += 1;
    }
    if prefix == old.len() && prefix == new.len() {
        return Ok(Update::Unchanged);
    }
    while !old.is_char_boundary(prefix) || !new.is_char_boundary(prefix) {
        prefix -= 1;
    }
    let mut suffix = 0;
    for (index, (a, b)) in old.as_bytes()[prefix..]
        .iter()
        .rev()
        .zip(new.as_bytes()[prefix..].iter().rev())
        .enumerate()
    {
        if index % 16384 == 0 {
            cancel.check()?;
        }
        if a != b {
            break;
        }
        suffix += 1;
    }
    while !old.is_char_boundary(old.len() - suffix) || !new.is_char_boundary(new.len() - suffix) {
        suffix -= 1;
    }
    cancel.check()?;
    Ok(Update::Replaced {
        old: prefix..old.len() - suffix,
        new: prefix..new.len() - suffix,
    })
}
