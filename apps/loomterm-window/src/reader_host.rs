//! Per-Window preparation seam for a private Markdown Reader slot.
//!
//! This drives Reader Core state and bounded loading only. A native document
//! surface, input bridge, fonts and pixel/resource renderer are separate work.
use loomterm_reader_core::{
    view::{
        Action, Frame, Input, LogicalSize, Metrics, ResourceFailure, ResourceTicket, ViewApply,
        ViewError, ViewLimits, ViewLoadRequest, ViewLoadResult, Viewport,
    },
    Cancellation, Limits, ReaderState,
};
use std::{
    path::Path,
    thread::{self, JoinHandle},
};

struct ActiveLoad {
    generation: u64,
    worker: JoinHandle<ViewLoadResult>,
}

struct PendingLoad {
    generation: u64,
    request: ViewLoadRequest,
}

#[derive(Debug)]
pub enum ReaderHostError {
    View(ViewError),
    GenerationExhausted,
    WorkerPanicked,
}

impl std::fmt::Display for ReaderHostError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::View(error) => write!(f, "Reader view: {error}"),
            Self::GenerationExhausted => f.write_str("Reader host generation exhausted"),
            Self::WorkerPanicked => f.write_str("Reader content worker panicked"),
        }
    }
}

impl std::error::Error for ReaderHostError {}

impl From<ViewError> for ReaderHostError {
    fn from(error: ViewError) -> Self {
        Self::View(error)
    }
}

/// One slot for this Window Client. It owns Reader state, never a work Terminal.
pub struct ReaderHost {
    generation: u64,
    viewport: Viewport,
    metrics: Metrics,
    view: Option<loomterm_reader_core::view::ReaderView>,
    frame: Option<Frame>,
    active: Option<ActiveLoad>,
    pending: Option<PendingLoad>,
}

impl ReaderHost {
    pub fn new(viewport: Viewport) -> Result<Self, ReaderHostError> {
        let viewport = Viewport::new(viewport.columns, viewport.rows)?;
        Ok(Self {
            generation: 0,
            viewport,
            metrics: Metrics::default(),
            view: None,
            frame: None,
            active: None,
            pending: None,
        })
    }

    /// Opens an explicitly selected local file, replacing this Window's slot.
    pub fn open(&mut self, path: &Path) -> Result<u64, ReaderHostError> {
        let generation = self.next_generation()?;
        if self.view.is_none() {
            self.view = Some(loomterm_reader_core::view::ReaderView::new(
                Limits::default(),
                ViewLimits::default(),
                self.viewport,
            )?);
            self.view
                .as_mut()
                .expect("Reader slot was just created")
                .set_metrics(self.metrics)?;
        }

        let request = self.view.as_mut().expect("Reader slot exists").open(path)?;
        self.generation = generation;
        self.frame = None;
        self.enqueue(generation, request);
        Ok(generation)
    }

    /// Starts an explicit reload without introducing a file watcher.
    pub fn refresh(&mut self) -> Result<bool, ReaderHostError> {
        let generation = self.next_generation()?;
        let Some(request) = self.view.as_mut().ok_or(ViewError::NoDocument)?.refresh()? else {
            return Ok(false);
        };
        self.generation = generation;
        self.frame = None;
        self.enqueue(generation, request);
        Ok(true)
    }

    fn next_generation(&self) -> Result<u64, ReaderHostError> {
        self.generation
            .checked_add(1)
            .ok_or(ReaderHostError::GenerationExhausted)
    }

    fn enqueue(&mut self, generation: u64, request: ViewLoadRequest) {
        if self.active.is_some() {
            // A single replaceable pending request bounds rapid open/refresh.
            self.pending = Some(PendingLoad {
                generation,
                request,
            });
        } else {
            self.active = Some(Self::start(generation, request));
        }
    }

    fn start(generation: u64, request: ViewLoadRequest) -> ActiveLoad {
        ActiveLoad {
            generation,
            worker: thread::spawn(move || request.execute()),
        }
    }

    /// Applies a finished worker result on the owning thread without blocking it.
    pub fn poll(&mut self) -> Result<Option<ViewApply>, ReaderHostError> {
        if !self
            .active
            .as_ref()
            .is_some_and(|active| active.worker.is_finished())
        {
            return Ok(None);
        }

        let active = self.active.take().expect("finished active load exists");
        let result = match active.worker.join() {
            Ok(result) => result,
            Err(_) => {
                self.start_pending();
                return Err(ReaderHostError::WorkerPanicked);
            }
        };
        let applied = if active.generation == self.generation {
            match self.view.as_mut() {
                Some(view) => {
                    self.frame = None;
                    view.apply(result)
                }
                None => ViewApply::Stale,
            }
        } else {
            drop(result);
            ViewApply::Stale
        };

        self.start_pending();
        Ok(Some(applied))
    }

    fn start_pending(&mut self) {
        if let Some(pending) = self.pending.take() {
            if pending.generation == self.generation && self.view.is_some() {
                self.active = Some(Self::start(pending.generation, pending.request));
            }
        }
    }

    pub fn is_loading(&self) -> bool {
        self.active.is_some() || self.pending.is_some()
    }

    pub fn resize(&mut self, viewport: Viewport) -> Result<(), ReaderHostError> {
        let viewport = Viewport::new(viewport.columns, viewport.rows)?;
        self.viewport = viewport;
        self.frame = None;
        if let Some(view) = &mut self.view {
            view.resize(viewport)?;
        }
        Ok(())
    }

    pub fn set_metrics(&mut self, metrics: Metrics) -> Result<(), ReaderHostError> {
        if let Some(view) = &mut self.view {
            view.set_metrics(metrics)?;
        } else {
            let mut validator = loomterm_reader_core::view::ReaderView::new(
                Limits::default(),
                ViewLimits::default(),
                self.viewport,
            )?;
            validator.set_metrics(metrics)?;
        }
        self.metrics = metrics;
        self.frame = None;
        Ok(())
    }

    pub fn input(
        &mut self,
        input: Input,
        cancel: &Cancellation,
    ) -> Result<Action, ReaderHostError> {
        self.frame = None;
        self.view
            .as_mut()
            .ok_or(ViewError::NoDocument)?
            .input(input, cancel)
            .map_err(Into::into)
    }

    pub fn frame(&mut self, cancel: &Cancellation) -> Result<Option<&Frame>, ReaderHostError> {
        self.frame = None;
        let Some(view) = &mut self.view else {
            return Ok(None);
        };
        let frame = view.frame(cancel)?;
        if view.is_current_frame(&frame) {
            self.frame = Some(frame);
        }
        Ok(self.frame.as_ref())
    }

    /// The renderer must re-check this lease immediately before painting.
    pub fn current_paint_frame(&self) -> Option<&Frame> {
        let frame = self.frame.as_ref()?;
        self.view.as_ref()?.is_current_frame(frame).then_some(frame)
    }

    pub fn selection(&self) -> Option<&str> {
        self.view.as_ref()?.selection()
    }

    pub fn error(&self) -> Option<&ViewError> {
        self.view.as_ref()?.error()
    }

    pub fn source_state(&self) -> Option<&ReaderState> {
        Some(self.view.as_ref()?.source_state())
    }

    pub fn resource_size(
        &mut self,
        ticket: &ResourceTicket,
        size: LogicalSize,
    ) -> Result<bool, ReaderHostError> {
        let changed = self
            .view
            .as_mut()
            .ok_or(ViewError::NoDocument)?
            .resource_size(ticket, size)?;
        if changed {
            self.frame = None;
        }
        Ok(changed)
    }

    pub fn resource_failed(
        &mut self,
        ticket: &ResourceTicket,
        failure: ResourceFailure,
    ) -> Result<bool, ReaderHostError> {
        let changed = self
            .view
            .as_mut()
            .ok_or(ViewError::NoDocument)?
            .resource_failed(ticket, failure)?;
        if changed {
            self.frame = None;
        }
        Ok(changed)
    }

    /// A confirmed parent-client close cancels content work and releases its frame.
    pub fn close(&mut self) -> Result<(), ReaderHostError> {
        let generation = self.next_generation()?;
        if let Some(view) = &mut self.view {
            view.cancel();
            view.close();
        }
        self.generation = generation;
        self.pending = None;
        self.frame = None;
        self.view = None;
        Ok(())
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }
}

impl Drop for ReaderHost {
    fn drop(&mut self) {
        if let Some(view) = &mut self.view {
            view.cancel();
            view.close();
        }
        self.pending = None;
        self.frame = None;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        sync::atomic::{AtomicU64, Ordering},
        time::{Duration, Instant},
    };

    static NEXT_DIR: AtomicU64 = AtomicU64::new(0);

    struct Files(std::path::PathBuf);

    impl Files {
        fn new() -> Self {
            let path = std::env::temp_dir().join(format!(
                "loomterm-reader-host-{}-{}",
                std::process::id(),
                NEXT_DIR.fetch_add(1, Ordering::Relaxed)
            ));
            fs::create_dir(&path).unwrap();
            Self(path)
        }

        fn write(&self, name: &str, text: &str) -> std::path::PathBuf {
            let path = self.0.join(name);
            fs::write(&path, text).unwrap();
            path
        }
    }

    impl Drop for Files {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    fn wait_loaded(host: &mut ReaderHost) {
        let deadline = Instant::now() + Duration::from_secs(3);
        while host.is_loading() && Instant::now() < deadline {
            host.poll().unwrap();
            thread::yield_now();
        }
        assert!(!host.is_loading(), "Reader worker did not finish");
    }

    fn visible_text(host: &mut ReaderHost) -> String {
        let frame = host.frame(&Cancellation::default()).unwrap().unwrap();
        frame
            .lines
            .iter()
            .flat_map(|line| line.clusters.iter())
            .map(|cluster| frame.text(cluster).to_owned())
            .collect()
    }

    #[test]
    fn replacement_coalesces_pending_target_and_keeps_frames_private() {
        let files = Files::new();
        let first = files.write("first.md", "first target");
        let skipped = files.write("skipped.md", "must not become the active view");
        let last = files.write("last.md", "final target");
        let viewport = Viewport::new(40, 10).unwrap();
        let mut host = ReaderHost::new(viewport).unwrap();

        host.open(&first).unwrap();
        host.open(&skipped).unwrap();
        host.open(&last).unwrap();
        assert!(host.is_loading());
        wait_loaded(&mut host);
        assert!(visible_text(&mut host).contains("final target"));
        assert!(!visible_text(&mut host).contains("must not"));

        host.resize(Viewport::new(24, 7).unwrap()).unwrap();
        assert!(host.current_paint_frame().is_none());
        assert!(host.frame(&Cancellation::default()).unwrap().is_some());
        host.input(Input::Page(1), &Cancellation::default())
            .unwrap();
        assert!(host.current_paint_frame().is_none());
        assert_eq!(host.generation(), 3);
        host.close().unwrap();
        assert!(host.current_paint_frame().is_none());
        assert_eq!(host.generation(), 4);
    }

    #[test]
    fn resource_resolution_invalidates_only_the_current_frame() {
        let files = Files::new();
        let path = files.write("image.md", "![fixture](image.png)");
        let mut host = ReaderHost::new(Viewport::new(40, 10).unwrap()).unwrap();
        host.open(&path).unwrap();
        wait_loaded(&mut host);

        let ticket = host
            .frame(&Cancellation::default())
            .unwrap()
            .unwrap()
            .resources[0]
            .ticket
            .clone();
        assert!(host.current_paint_frame().is_some());
        assert!(host
            .resource_size(
                &ticket,
                LogicalSize {
                    columns: 4,
                    rows: 2,
                }
            )
            .unwrap());
        assert!(host.current_paint_frame().is_none());
        assert!(host.frame(&Cancellation::default()).unwrap().is_some());
    }

    #[test]
    fn bounded_projection_finishes_across_pending_frames() {
        let files = Files::new();
        let content = (0..500)
            .map(|index| format!("Paragraph {index:04}: fixed content.\n\n"))
            .collect::<String>();
        let path = files.write("many.md", &content);
        let mut host = ReaderHost::new(Viewport::new(60, 20).unwrap()).unwrap();
        host.open(&path).unwrap();
        wait_loaded(&mut host);
        let mut pending = 0;
        loop {
            let frame = host.frame(&Cancellation::default()).unwrap().unwrap();
            if !frame.pending {
                break;
            }
            pending += 1;
            assert!(pending < 200, "bounded projection did not complete");
        }
        assert!(pending > 0, "fixture did not exercise pending projection");
        assert!(visible_text(&mut host).contains("Paragraph 0000"));
    }
}
