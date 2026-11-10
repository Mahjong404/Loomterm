//! Renderer-independent projection, bounded logical layout, private viewport and input intents.
//! Content/projection jobs run on a bounded worker. A frame contains only visible draw data.
//!
//! ```no_run
//! use loomterm_reader_core::{view::*, Cancellation, Limits};
//! # fn main() -> Result<(), ViewError> {
//! let mut view = ReaderView::new(Limits::default(), ViewLimits::default(), Viewport::new(80, 30)?)?;
//! let job = view.open(std::path::Path::new("selected.md"))?;
//! // A real adapter moves execute() and bounded frame layout to its content worker.
//! view.apply(job.execute());
//! let frame = view.frame(&Cancellation::default())?;
//! if view.is_current_frame(&frame) {
//!     for line in &frame.lines {
//!         for glyph in &line.clusters { let _literal_text = frame.text(glyph); }
//!     }
//! }
//! view.input(Input::Page(1), &Cancellation::default())?;
//! view.close();
//! # Ok(())
//! # }
//! ```
mod layout;
mod model;
mod project;
mod state;
use crate::{
    Apply, Cancellation, Limits, LoadRequest, LoadResult, Reader, ReaderError, ReaderState, Update,
};
pub use model::*;
pub use project::Projection;
use std::{path::Path, sync::Arc};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ViewApply {
    Ready,
    Stale,
    Failed(ViewError),
    Cancelled,
}

/// Exactly one content/view slot. The Window adapter owns window lifetime and task scheduling.
pub struct ReaderView {
    reader: Reader,
    state: Option<state::ViewState>,
    viewport: Viewport,
    metrics: Metrics,
    limits: ViewLimits,
    error: Option<ViewError>,
}
impl ReaderView {
    pub fn new(core: Limits, limits: ViewLimits, viewport: Viewport) -> Result<Self, ViewError> {
        limits.validate()?;
        Viewport::new(viewport.columns, viewport.rows)?;
        Ok(Self {
            reader: Reader::new(core)?,
            state: None,
            viewport,
            metrics: Metrics::default(),
            limits,
            error: None,
        })
    }
    pub fn open(&mut self, path: &Path) -> Result<ViewLoadRequest, ViewError> {
        let previous = self.state.as_ref().map(|s| s.projection.clone());
        let core = self.reader.open(path)?;
        if self.reader.document().is_none() {
            self.state = None;
        }
        self.error = None;
        Ok(ViewLoadRequest {
            core,
            previous,
            limits: self.limits,
        })
    }
    pub fn refresh(&mut self) -> Result<Option<ViewLoadRequest>, ViewError> {
        Ok(self.reader.reload()?.map(|core| ViewLoadRequest {
            core,
            previous: self.state.as_ref().map(|s| s.projection.clone()),
            limits: self.limits,
        }))
    }
    pub fn poll(&mut self) -> Result<Option<ViewLoadRequest>, ViewError> {
        Ok(self.reader.poll()?.map(|core| ViewLoadRequest {
            core,
            previous: self.state.as_ref().map(|s| s.projection.clone()),
            limits: self.limits,
        }))
    }
    pub fn set_following(&mut self, follow: bool) {
        self.reader.set_following(follow);
    }
    pub fn cancel(&mut self) {
        self.reader.cancel();
    }
    pub fn close(&mut self) {
        self.reader.clear();
        self.state = None;
        self.error = None;
    }
    pub fn source_state(&self) -> &ReaderState {
        self.reader.state()
    }
    pub fn error(&self) -> Option<&ViewError> {
        self.error.as_ref()
    }
    pub fn projection(&self) -> Option<&Arc<Projection>> {
        self.state.as_ref().map(|s| &s.projection)
    }
    pub fn apply(&mut self, result: ViewLoadResult) -> ViewApply {
        let update = result.core.update().cloned();
        match self.reader.apply(result.core) {
            Apply::Stale => ViewApply::Stale,
            Apply::Cancelled => ViewApply::Cancelled,
            Apply::Failed(error) => {
                let error = ViewError::Core(error);
                self.error = Some(error.clone());
                ViewApply::Failed(error)
            }
            Apply::Ready(_) => match result.projection {
                Err(error) => {
                    self.error = Some(error.clone());
                    ViewApply::Failed(error)
                }
                Ok(projection) => {
                    if !self
                        .state
                        .as_ref()
                        .is_some_and(|s| Arc::ptr_eq(&s.projection, &projection))
                    {
                        let anchor = self.state.as_ref().map(|s| s.anchor());
                        let mut state = state::ViewState::new(
                            projection,
                            self.viewport,
                            self.metrics,
                            self.limits,
                        );
                        if let Some(anchor) = anchor {
                            let byte = match &update {
                                Some(Update::Replaced { old, new }) if anchor.byte >= old.end => {
                                    anchor.byte.saturating_add_signed(
                                        new.len() as isize - old.len() as isize,
                                    )
                                }
                                Some(Update::Replaced { old, new }) if anchor.byte >= old.start => {
                                    new.start + anchor.byte.saturating_sub(old.start).min(new.len())
                                }
                                _ => anchor.byte,
                            };
                            let precision = if matches!(&update,Some(Update::Replaced { old,.. }) if old.start<=anchor.byte && anchor.byte<old.end)
                            {
                                Precision::Approximate
                            } else {
                                anchor.precision
                            };
                            state.restore_source(byte, precision);
                        }
                        self.state = Some(state);
                    }
                    self.error = None;
                    ViewApply::Ready
                }
            },
        }
    }
    pub fn resize(&mut self, viewport: Viewport) -> Result<(), ViewError> {
        Viewport::new(viewport.columns, viewport.rows)?;
        if viewport != self.viewport {
            self.viewport = viewport;
            if let Some(state) = &mut self.state {
                state.resize(viewport, self.metrics);
            }
        }
        Ok(())
    }
    pub fn set_metrics(&mut self, metrics: Metrics) -> Result<(), ViewError> {
        metrics.validate()?;
        if metrics != self.metrics {
            self.metrics = metrics;
            if let Some(state) = &mut self.state {
                state.resize(self.viewport, metrics);
            }
        }
        Ok(())
    }
    pub fn frame(&mut self, cancel: &Cancellation) -> Result<Frame, ViewError> {
        let result = self
            .state
            .as_mut()
            .ok_or(ViewError::NoDocument)?
            .frame(cancel);
        if let Err(error) = &result {
            self.error = Some(error.clone());
        }
        result
    }
    pub fn input(&mut self, input: Input, cancel: &Cancellation) -> Result<Action, ViewError> {
        self.state
            .as_mut()
            .ok_or(ViewError::NoDocument)?
            .input(input, cancel)
    }
    pub fn resource_size(
        &mut self,
        ticket: &ResourceTicket,
        size: LogicalSize,
    ) -> Result<bool, ViewError> {
        self.state
            .as_mut()
            .ok_or(ViewError::NoDocument)?
            .resource_size(ticket, size)
    }
    pub fn resource_failed(
        &mut self,
        ticket: &ResourceTicket,
        failure: ResourceFailure,
    ) -> Result<bool, ViewError> {
        self.state
            .as_mut()
            .ok_or(ViewError::NoDocument)?
            .resource_failed(ticket, failure)
    }
    pub fn resource_status(&self, node: crate::NodeId) -> Option<ResourceStatus> {
        self.state.as_ref()?.resource_status(node)
    }
    pub fn is_current_frame(&self, frame: &Frame) -> bool {
        self.state
            .as_ref()
            .is_some_and(|s| s.is_current_frame(frame))
    }
    pub fn selection(&self) -> Option<&str> {
        self.state.as_ref().and_then(|s| s.selection())
    }
    pub fn focused_link(&self) -> Option<crate::NodeId> {
        self.state
            .as_ref()
            .and_then(|s| s.focused_link.map(|index| s.projection.links[index]))
    }
    pub fn locate_search(
        &mut self,
        hit: &SearchHit,
        cancel: &Cancellation,
    ) -> Result<(), ViewError> {
        self.state
            .as_mut()
            .ok_or(ViewError::NoDocument)?
            .locate_search(hit, cancel)
    }
}

pub struct ViewLoadRequest {
    core: LoadRequest,
    previous: Option<Arc<Projection>>,
    limits: ViewLimits,
}
impl ViewLoadRequest {
    pub fn cancellation(&self) -> Cancellation {
        self.core.cancellation()
    }
    pub fn execute(self) -> ViewLoadResult {
        let cancel = self.core.cancellation();
        let core = self.core.execute();
        let projection = match core.document() {
            Some(document) => match self.previous {
                Some(previous) if Arc::ptr_eq(&previous.document.content, &document.content) => {
                    Ok(previous)
                }
                _ => Projection::build(document.clone(), self.limits, &cancel).map(Arc::new),
            },
            None => Err(ViewError::Core(ReaderError::Cancelled)), // apply(core) determines the actual source error.
        };
        ViewLoadResult { core, projection }
    }
}
pub struct ViewLoadResult {
    core: LoadResult,
    projection: Result<Arc<Projection>, ViewError>,
}
