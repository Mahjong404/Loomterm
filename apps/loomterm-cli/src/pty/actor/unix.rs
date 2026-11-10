// Modified for Loomterm (2026-11-05); see apps/loomterm-cli/PATCHES.md.
use std::{
    collections::VecDeque,
    io::{Read, Write},
    os::fd::{AsRawFd, OwnedFd, RawFd},
    sync::{mpsc as std_mpsc, Arc, Mutex},
    time::{Duration, Instant},
};

use bytes::Bytes;
use tokio::sync::mpsc::{self, error::TryRecvError as DataTryRecvError};
use tracing::{debug, warn};

use crate::pty::budget::{self, Budget, IoFailure, Permit};
use crate::pty::fd;
use std::sync::atomic::{AtomicBool, Ordering};

// Actor handle methods must call wake_actor() after queuing work. The idle
// timeout is only a fallback for missed wakes; PTY and wake readiness drive
// normal responsiveness.
const ACTOR_IDLE_POLL_MS: i32 = 1000;
const ACTOR_COMMAND_BUFFER: usize = 128;
const HANDOFF_DRAIN_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum ActorState {
    Running,
    Quiesced,
    Released,
}

pub(crate) struct PtyReadResult {
    pub terminal_responses: Vec<Bytes>,
    pub failure: Option<String>,
}

impl PtyReadResult {
    #[cfg(test)]
    pub(crate) fn empty() -> Self {
        Self {
            terminal_responses: Vec::new(),
            failure: None,
        }
    }
}

type ReadCallback = Box<dyn FnMut(&[u8]) -> PtyReadResult + Send + 'static>;
type ReaderExitCallback = Box<dyn FnOnce() + Send + 'static>;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct PtyResize {
    rows: u16,
    cols: u16,
    cell_width_px: u32,
    cell_height_px: u32,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct PtyResizeRequest {
    resize: PtyResize,
    terminal_responses: Vec<Bytes>,
}

#[derive(Default)]
struct SharedPtyControls {
    resize: Option<PtyResizeRequest>,
    nudge: Option<PtyResize>,
    terminal_responses: Vec<Bytes>,
    focus: Option<Bytes>,
}

pub(crate) struct PtyIoActorConfig {
    pub pane_id: u32,
    pub master_fd: OwnedFd,
    pub initially_quiesced: bool,
    pub on_read: ReadCallback,
    pub on_reader_exit: Option<ReaderExitCallback>,
}

enum PtyIoDataCommand {
    WriteUserInput(Bytes, Option<Permit>),
    SubmitUserInput {
        text: Bytes,
        enter: Bytes,
        delay: Duration,
        reply: std_mpsc::Sender<std::io::Result<()>>,
        _permit: Option<Permit>,
    },
}

enum PtyIoControlCommand {
    BeginHandoff(std_mpsc::Sender<std::io::Result<()>>),
    DuplicateForHandoff(std_mpsc::Sender<std::io::Result<RawFd>>),
    ForegroundProcessGroup(std_mpsc::Sender<Option<u32>>),
    RollbackHandoff(std_mpsc::Sender<std::io::Result<()>>),
    ReleaseAfterCommit(std_mpsc::Sender<std::io::Result<()>>),
    Shutdown,
}

#[derive(Clone)]
pub(crate) struct PtyIoActorHandle {
    data_tx: mpsc::Sender<PtyIoDataCommand>,
    control_tx: std_mpsc::SyncSender<PtyIoControlCommand>,
    wake: fd::WakeWriter,
    user_writes: Arc<Mutex<UserWriteGate>>,
    controls: Arc<Mutex<SharedPtyControls>>,
    response_order: Arc<Mutex<()>>,
    input_guard: crate::platform::PtyInputGuard,
    input_budget: Arc<Budget>,
    failure: IoFailure,
    stopped: Arc<AtomicBool>,
}

#[derive(Debug)]
struct UserWriteGate {
    accepting: bool,
}

impl PtyIoActorHandle {
    pub(crate) fn io_failure(&self) -> Option<String> {
        self.failure.reason()
    }
    pub(crate) fn write_focus_event(&self, bytes: Bytes) -> std::io::Result<()> {
        if bytes.len() > 64 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "focus encoding exceeds 64 bytes",
            ));
        }
        if self.stopped.load(Ordering::Acquire) || self.failure.reason().is_some() {
            return Err(input_submission_closed_error());
        }
        let accepting = self.user_writes.lock().unwrap_or_else(|p| p.into_inner());
        if !accepting.accepting {
            return Err(input_submission_closed_error());
        }
        self.controls
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .focus = Some(bytes);
        self.wake_actor();
        Ok(())
    }
    pub(crate) fn validate_input_submission(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.input_guard.validate(bytes)
    }

    pub(crate) fn try_write_user_input(
        &self,
        bytes: Bytes,
    ) -> Result<(), mpsc::error::TrySendError<Bytes>> {
        let user_writes = self
            .user_writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.failure.reason().is_some() || !user_writes.accepting {
            return Err(mpsc::error::TrySendError::Closed(bytes));
        }
        if bytes.len() > budget::MAX_USER_WRITE_BYTES {
            return Err(mpsc::error::TrySendError::Full(bytes));
        }
        let Some(permit) = self.input_budget.acquire(bytes.len()) else {
            return Err(mpsc::error::TrySendError::Full(bytes));
        };
        match self
            .data_tx
            .try_send(PtyIoDataCommand::WriteUserInput(bytes, Some(permit)))
        {
            Ok(()) => {
                self.wake_actor();
                Ok(())
            }
            Err(mpsc::error::TrySendError::Full(command)) => {
                let PtyIoDataCommand::WriteUserInput(bytes, _permit) = command else {
                    unreachable!("queued write returned another command")
                };
                Err(mpsc::error::TrySendError::Full(bytes))
            }
            Err(mpsc::error::TrySendError::Closed(command)) => {
                let PtyIoDataCommand::WriteUserInput(bytes, _permit) = command else {
                    unreachable!("queued write returned another command")
                };
                Err(mpsc::error::TrySendError::Closed(bytes))
            }
        }
    }

    pub(crate) fn queue_user_input_submission(
        &self,
        text: Bytes,
        enter: Bytes,
        delay: Duration,
    ) -> std::io::Result<std_mpsc::Receiver<std::io::Result<()>>> {
        let user_writes = self
            .user_writes
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        if self.failure.reason().is_some() || !user_writes.accepting {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "pty actor closed",
            ));
        }
        let size = text
            .len()
            .checked_add(enter.len())
            .filter(|size| *size <= 1024 * 1024)
            .ok_or_else(|| {
                std::io::Error::new(
                    std::io::ErrorKind::InvalidInput,
                    "input submission exceeds 1 MiB",
                )
            })?;
        let permit = self.input_budget.acquire(size).ok_or_else(|| {
            std::io::Error::new(std::io::ErrorKind::WouldBlock, "pty input budget exhausted")
        })?;
        let (reply_tx, reply_rx) = std_mpsc::channel();
        self.data_tx
            .try_send(PtyIoDataCommand::SubmitUserInput {
                text,
                enter,
                delay,
                reply: reply_tx,
                _permit: Some(permit),
            })
            .map_err(|err| match err {
                mpsc::error::TrySendError::Full(_) => {
                    std::io::Error::new(std::io::ErrorKind::WouldBlock, "pty input queue is full")
                }
                mpsc::error::TrySendError::Closed(_) => {
                    std::io::Error::new(std::io::ErrorKind::BrokenPipe, "pty actor closed")
                }
            })?;
        self.wake_actor();
        Ok(reply_rx)
    }

    pub(crate) fn write_terminal_response(&self, response: impl FnOnce() -> Option<Bytes>) {
        let _order = self
            .response_order
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner());
        let Some(bytes) = response() else {
            return;
        };
        if !bytes.is_empty() {
            let mut controls = self.controls.lock().unwrap_or_else(|p| p.into_inner());
            if !append_control_responses(&mut controls, vec![bytes]) {
                self.failure.fail("PTY control reply budget exhausted");
            }
            drop(controls);
            self.wake_actor();
        }
    }

    pub(crate) fn resize(
        &self,
        rows: u16,
        cols: u16,
        cell_width_px: u32,
        cell_height_px: u32,
        terminal_responses: Vec<Bytes>,
    ) {
        {
            let mut controls = self
                .controls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            // Resize geometry coalesces; replies retain their original admission order.
            if !append_control_responses(&mut controls, terminal_responses) {
                self.failure.fail("PTY resize reply budget exhausted");
            }
            controls.resize = Some(PtyResizeRequest {
                resize: PtyResize {
                    rows,
                    cols,
                    cell_width_px,
                    cell_height_px,
                },
                terminal_responses: Vec::new(),
            });
        }
        self.wake_actor();
    }

    pub(crate) fn nudge_child_redraw_after_handoff(
        &self,
        rows: u16,
        cols: u16,
        cell_width_px: u32,
        cell_height_px: u32,
    ) {
        {
            let mut controls = self
                .controls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            controls.nudge = Some(PtyResize {
                rows,
                cols,
                cell_width_px,
                cell_height_px,
            });
        }
        self.wake_actor();
    }

    pub(crate) fn begin_handoff(&self, timeout: Duration) -> std::io::Result<()> {
        let (reply_tx, reply_rx) = std_mpsc::channel();
        {
            let mut user_writes = self
                .user_writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            if !user_writes.accepting {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::WouldBlock,
                    "PTY handoff is already in progress",
                ));
            }
            user_writes.accepting = false;
            if let Err(err) = self
                .control_tx
                .try_send(PtyIoControlCommand::BeginHandoff(reply_tx))
            {
                user_writes.accepting = true;
                return Err(control_send_error(err));
            }
            self.wake_actor();
        }
        match reply_rx.recv_timeout(timeout) {
            Ok(Ok(())) => Ok(()),
            Ok(Err(err)) => {
                let _ = self.rollback_handoff();
                Err(err)
            }
            Err(_) => {
                let _ = self.rollback_handoff();
                Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out waiting for PTY actor to quiesce",
                ))
            }
        }
    }

    pub(crate) fn duplicate_for_handoff(&self) -> std::io::Result<RawFd> {
        let (reply_tx, reply_rx) = std_mpsc::channel();
        self.control_tx
            .try_send(PtyIoControlCommand::DuplicateForHandoff(reply_tx))
            .map_err(control_send_error)?;
        self.wake_actor();
        reply_rx.recv_timeout(Duration::from_secs(1)).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out waiting for PTY handoff duplicate",
            )
        })?
    }

    pub(crate) fn foreground_process_group_id(&self) -> Option<u32> {
        let (reply_tx, reply_rx) = std_mpsc::channel();
        self.control_tx
            .try_send(PtyIoControlCommand::ForegroundProcessGroup(reply_tx))
            .ok()?;
        self.wake_actor();
        reply_rx.recv_timeout(Duration::from_secs(1)).ok()?
    }

    pub(crate) fn rollback_handoff(&self) -> std::io::Result<()> {
        let (reply_tx, reply_rx) = std_mpsc::channel();
        self.control_tx
            .try_send(PtyIoControlCommand::RollbackHandoff(reply_tx))
            .map_err(control_send_error)?;
        self.wake_actor();
        let result = reply_rx.recv_timeout(Duration::from_secs(1)).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out waiting for PTY handoff rollback",
            )
        })?;
        if result.is_ok() {
            let mut user_writes = self
                .user_writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            user_writes.accepting = true;
        }
        result
    }

    pub(crate) fn release_after_commit(&self) -> std::io::Result<()> {
        {
            let mut user_writes = self
                .user_writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            user_writes.accepting = false;
        }
        let (reply_tx, reply_rx) = std_mpsc::channel();
        self.control_tx
            .try_send(PtyIoControlCommand::ReleaseAfterCommit(reply_tx))
            .map_err(control_send_error)?;
        self.wake_actor();
        reply_rx.recv_timeout(Duration::from_secs(1)).map_err(|_| {
            std::io::Error::new(
                std::io::ErrorKind::TimedOut,
                "timed out waiting for PTY actor release",
            )
        })?
    }

    pub(crate) fn shutdown(&self) {
        {
            let mut user_writes = self
                .user_writes
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            user_writes.accepting = false;
        }
        self.stopped.store(true, Ordering::Release);
        let _ = self.control_tx.try_send(PtyIoControlCommand::Shutdown);
        self.wake_actor();
    }

    fn wake_actor(&self) {
        if let Err(err) = self.wake.wake() {
            debug!(err = %err, "failed to wake PTY actor");
        }
    }
}

pub(crate) struct PtyIoActor;

impl PtyIoActor {
    #[cfg(test)]
    pub(crate) fn spawn(config: PtyIoActorConfig) -> std::io::Result<PtyIoActorHandle> {
        Self::spawn_inner(config, None, None)
    }

    pub(crate) fn spawn_with_admission(
        config: PtyIoActorConfig,
        session_admission: Option<Arc<Permit>>,
    ) -> std::io::Result<PtyIoActorHandle> {
        Self::spawn_inner(config, None, session_admission)
    }

    fn spawn_inner(
        config: PtyIoActorConfig,
        poll_observer: Option<std_mpsc::Sender<()>>,
        session_admission: Option<Arc<Permit>>,
    ) -> std::io::Result<PtyIoActorHandle> {
        fd::set_cloexec(config.master_fd.as_raw_fd())?;
        fd::set_nonblocking(config.master_fd.as_raw_fd())?;

        let (data_tx, data_rx) = mpsc::channel(ACTOR_COMMAND_BUFFER);
        let (control_tx, control_rx) = std_mpsc::sync_channel(16);
        let wake_pipe = fd::create_wake_pipe()?;
        let user_writes = Arc::new(Mutex::new(UserWriteGate {
            accepting: !config.initially_quiesced,
        }));
        let controls = Arc::new(Mutex::new(SharedPtyControls::default()));
        let response_order = Arc::new(Mutex::new(()));
        let file = Arc::new(std::fs::File::from(config.master_fd));
        let failure = IoFailure::default();
        let stopped = Arc::new(AtomicBool::new(false));
        let handle = PtyIoActorHandle {
            input_budget: budget::input_budget(),
            failure: failure.clone(),
            stopped: stopped.clone(),
            data_tx,
            control_tx,
            wake: wake_pipe.writer,
            user_writes,
            controls: Arc::clone(&controls),
            response_order: Arc::clone(&response_order),
            input_guard: crate::platform::PtyInputGuard::new(&file),
        };

        let mut runner = PtyIoActorRunner {
            _session_admission: session_admission,
            reply_budget: budget::reply_budget(),
            failure,
            stopped,
            pane_id: config.pane_id,
            file,
            data_rx,
            control_rx,
            state: if config.initially_quiesced {
                ActorState::Quiesced
            } else {
                ActorState::Running
            },
            pending_writes: VecDeque::new(),
            current_write_offset: 0,
            active_submission: None,
            pending_handoff: None,
            wake_read_fd: wake_pipe.read_fd,
            controls,
            response_order,
            on_read: config.on_read,
            on_reader_exit: config.on_reader_exit,
            poll_observer,
        };
        std::thread::Builder::new()
            .name(format!("herdr-pty-{}", config.pane_id))
            .spawn(move || runner.run())
            .map_err(|err| std::io::Error::other(err.to_string()))?;

        Ok(handle)
    }

    #[cfg(test)]
    fn spawn_with_poll_observer(
        config: PtyIoActorConfig,
        poll_observer: std_mpsc::Sender<()>,
    ) -> std::io::Result<PtyIoActorHandle> {
        Self::spawn_inner(config, Some(poll_observer), None)
    }
}

struct PtyIoActorRunner {
    _session_admission: Option<Arc<Permit>>,
    reply_budget: Arc<Budget>,
    failure: IoFailure,
    stopped: Arc<AtomicBool>,
    pane_id: u32,
    file: Arc<std::fs::File>,
    data_rx: mpsc::Receiver<PtyIoDataCommand>,
    control_rx: std_mpsc::Receiver<PtyIoControlCommand>,
    state: ActorState,
    pending_writes: VecDeque<PendingWrite>,
    current_write_offset: usize,
    active_submission: Option<ActiveSubmission>,
    pending_handoff: Option<std_mpsc::Sender<std::io::Result<()>>>,
    wake_read_fd: OwnedFd,
    controls: Arc<Mutex<SharedPtyControls>>,
    response_order: Arc<Mutex<()>>,
    on_read: ReadCallback,
    on_reader_exit: Option<ReaderExitCallback>,
    poll_observer: Option<std_mpsc::Sender<()>>,
}

struct ActiveSubmission {
    _permit: Option<Permit>,
    enter: Bytes,
    delay: Duration,
    phase: SubmissionPhase,
    reply: std_mpsc::Sender<std::io::Result<()>>,
}

#[derive(Debug)]
struct PendingWrite {
    focus: bool,
    _permit: Option<Permit>,
    bytes: Bytes,
    boundary: Option<SubmissionBoundary>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum SubmissionBoundary {
    Text,
    Enter,
}

enum SubmissionPhase {
    WritingText,
    WaitingUntil(Instant),
    WritingEnter,
}

impl PartialEq for PendingWrite {
    fn eq(&self, other: &Self) -> bool {
        self.bytes == other.bytes && self.boundary == other.boundary && self.focus == other.focus
    }
}
impl Eq for PendingWrite {}

fn append_control_responses(controls: &mut SharedPtyControls, bytes: Vec<Bytes>) -> bool {
    let total = controls
        .terminal_responses
        .iter()
        .chain(bytes.iter())
        .try_fold(0usize, |sum, bytes| {
            (bytes.len() <= 64 * 1024).then_some(())?;
            sum.checked_add(bytes.len())
        });
    if controls
        .terminal_responses
        .len()
        .saturating_add(bytes.len())
        > 64
        || total.is_none_or(|size| size > 256 * 1024)
    {
        return false;
    }
    controls.terminal_responses.extend(bytes);
    true
}

impl PtyIoActorRunner {
    fn enqueue_write(&mut self, bytes: Bytes) {
        if bytes.is_empty() {
            return;
        }
        let Some(permit) = (bytes.len() <= 64 * 1024)
            .then(|| self.reply_budget.acquire(bytes.len()))
            .flatten()
        else {
            self.failure.fail("PTY reply budget exhausted");
            return;
        };
        self.enqueue_admitted_write(bytes, Some(permit));
    }

    fn enqueue_admitted_write(&mut self, bytes: Bytes, permit: Option<Permit>) {
        if !bytes.is_empty() {
            self.pending_writes.push_back(PendingWrite {
                focus: false,
                bytes,
                boundary: None,
                _permit: permit,
            });
        }
    }

    fn enqueue_submission_write(&mut self, bytes: Bytes, boundary: SubmissionBoundary) {
        if !bytes.is_empty() {
            self.pending_writes.push_back(PendingWrite {
                focus: false,
                bytes,
                boundary: Some(boundary),
                _permit: None,
            });
        }
    }

    fn run(&mut self) {
        let mut should_exit = false;
        while !should_exit {
            if self.stopped.load(Ordering::Acquire) || self.failure.reason().is_some() {
                break;
            }
            should_exit = self.drain_commands();
            if should_exit || self.state == ActorState::Released {
                break;
            }

            self.apply_pending_controls();

            if !self.pending_writes.is_empty() {
                match self.flush_pending_writes_once() {
                    Ok(Some(boundary)) => self.complete_submission_boundary(boundary),
                    Ok(None) => {}
                    Err(err) => {
                        self.failure.fail(format!("PTY writer failed: {err}"));
                        self.fail_active_submission(err);
                        break;
                    }
                }
            }
            self.schedule_submission_enter();
            if self.active_submission.is_none() && self.pending_handoff.is_some() {
                continue;
            }

            if let Some(poll_observer) = &self.poll_observer {
                let _ = poll_observer.send(());
            }

            match fd::poll_pty_and_wake(
                self.file.as_raw_fd(),
                self.wake_read_fd.as_raw_fd(),
                self.state == ActorState::Running,
                !self.pending_writes.is_empty(),
                self.poll_timeout_ms(),
            ) {
                Ok(readiness) => {
                    if readiness.wake_ready {
                        if let Err(err) = fd::drain_wake_fd(self.wake_read_fd.as_raw_fd()) {
                            debug!(pane = self.pane_id, err = %err, "PTY actor wake drain failed");
                            break;
                        }
                        continue;
                    }
                    if self.state == ActorState::Running
                        && readiness.pty_read_ready
                        && !self.read_once()
                    {
                        break;
                    }
                    if readiness.pty_write_ready && !self.pending_writes.is_empty() {
                        match self.flush_pending_writes_once() {
                            Ok(Some(boundary)) => self.complete_submission_boundary(boundary),
                            Ok(None) => {}
                            Err(err) => {
                                self.failure.fail(format!("PTY writer failed: {err}"));
                                self.fail_active_submission(err);
                                break;
                            }
                        }
                    }
                }
                Err(err) => {
                    self.failure.fail(format!("PTY poll failed: {err}"));
                    debug!(pane = self.pane_id, err = %err, "PTY actor poll failed");
                    break;
                }
            }
        }

        self.close_input_queue();
        if let Some(on_reader_exit) = self.on_reader_exit.take() {
            on_reader_exit();
        }
        debug!(pane = self.pane_id, "PTY actor exiting");
    }

    fn drain_commands(&mut self) -> bool {
        if self.drain_control_commands() {
            return true;
        }
        if self.active_submission.is_some() {
            return false;
        }
        if let Some(reply) = self.pending_handoff.take() {
            self.defer_or_begin_handoff(reply);
            return false;
        }
        self.drain_data_commands()
    }

    fn drain_control_commands(&mut self) -> bool {
        let mut should_exit = false;
        loop {
            match self.control_rx.try_recv() {
                Ok(command) => {
                    if self.handle_control_command(command) {
                        should_exit = true;
                        break;
                    }
                }
                Err(std_mpsc::TryRecvError::Empty) => break,
                Err(std_mpsc::TryRecvError::Disconnected) => {
                    should_exit = true;
                    break;
                }
            }
        }
        should_exit
    }

    fn drain_data_commands(&mut self) -> bool {
        let mut should_exit = false;
        loop {
            match self.data_rx.try_recv() {
                Ok(command) => {
                    if self.handle_data_command(command) {
                        should_exit = true;
                        break;
                    }
                    if self.active_submission.is_some() {
                        break;
                    }
                }
                Err(DataTryRecvError::Empty) => break,
                Err(DataTryRecvError::Disconnected) => {
                    should_exit = true;
                    break;
                }
            }
        }
        should_exit
    }

    fn handle_data_command(&mut self, command: PtyIoDataCommand) -> bool {
        match command {
            PtyIoDataCommand::WriteUserInput(bytes, permit) => {
                if self.state == ActorState::Running {
                    self.enqueue_admitted_write(bytes, permit);
                }
            }
            PtyIoDataCommand::SubmitUserInput {
                text,
                enter,
                delay,
                reply,
                _permit,
            } => {
                if self.state == ActorState::Running {
                    let phase = if text.is_empty() {
                        SubmissionPhase::WaitingUntil(Instant::now() + delay)
                    } else {
                        self.enqueue_submission_write(text, SubmissionBoundary::Text);
                        SubmissionPhase::WritingText
                    };
                    self.active_submission = Some(ActiveSubmission {
                        _permit,
                        enter,
                        delay,
                        phase,
                        reply,
                    });
                } else {
                    let _ = reply.send(Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "pty actor is not accepting input",
                    )));
                }
            }
        }
        false
    }

    fn handle_control_command(&mut self, command: PtyIoControlCommand) -> bool {
        match command {
            PtyIoControlCommand::BeginHandoff(reply) => {
                self.defer_or_begin_handoff(reply);
            }
            PtyIoControlCommand::DuplicateForHandoff(reply) => {
                let result = if self.state == ActorState::Quiesced {
                    fd::duplicate_cloexec_fd(self.file.as_raw_fd())
                } else {
                    Err(std::io::Error::other(
                        "PTY actor must be quiesced before handoff duplication",
                    ))
                };
                let _ = reply.send(result);
            }
            PtyIoControlCommand::ForegroundProcessGroup(reply) => {
                let result =
                    crate::platform::foreground_process_group_id_for_tty_fd(self.file.as_raw_fd());
                let _ = reply.send(result);
            }
            PtyIoControlCommand::RollbackHandoff(reply) => {
                self.pending_handoff.take();
                let result = if self.state == ActorState::Released {
                    Err(std::io::Error::new(
                        std::io::ErrorKind::BrokenPipe,
                        "PTY actor was released before handoff rollback",
                    ))
                } else {
                    self.state = ActorState::Running;
                    Ok(())
                };
                let _ = reply.send(result);
            }
            PtyIoControlCommand::ReleaseAfterCommit(reply) => {
                self.state = ActorState::Released;
                self.pending_writes.clear();
                let _ = reply.send(Ok(()));
                return true;
            }
            PtyIoControlCommand::Shutdown => return true,
        }
        false
    }

    fn defer_or_begin_handoff(&mut self, reply: std_mpsc::Sender<std::io::Result<()>>) {
        if self.active_submission.is_none() {
            self.drain_pre_quiesce_commands();
        }
        if self.active_submission.is_some() {
            self.pending_handoff = Some(reply);
        } else {
            let result = self.begin_handoff();
            let _ = reply.send(result);
        }
    }

    fn begin_handoff(&mut self) -> std::io::Result<()> {
        self.drain_pre_quiesce_commands();
        if self.active_submission.is_some() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::WouldBlock,
                "PTY input submission is still in progress",
            ));
        }
        self.apply_pending_controls();
        if self.state == ActorState::Released {
            return Err(std::io::Error::new(
                std::io::ErrorKind::BrokenPipe,
                "PTY actor was released before handoff quiesce",
            ));
        }
        let deadline = Instant::now() + HANDOFF_DRAIN_TIMEOUT;
        let _ = self.flush_pending_writes_once()?;
        while !self.pending_writes.is_empty() {
            let remaining = deadline.saturating_duration_since(Instant::now());
            if remaining.is_zero() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "timed out draining PTY writes before handoff",
                ));
            }
            let timeout_ms = remaining.as_millis().min(i32::MAX as u128) as i32;
            let readiness = fd::poll_pty_and_wake(
                self.file.as_raw_fd(),
                self.wake_read_fd.as_raw_fd(),
                true,
                true,
                timeout_ms,
            )?;
            if readiness.wake_ready {
                fd::drain_wake_fd(self.wake_read_fd.as_raw_fd())?;
            }
            if readiness.pty_read_ready && !self.read_once() {
                return Err(std::io::Error::new(
                    std::io::ErrorKind::BrokenPipe,
                    "PTY closed while draining writes before handoff",
                ));
            }
            if readiness.pty_write_ready {
                let _ = self.flush_pending_writes_once()?;
            }
        }
        self.state = ActorState::Quiesced;
        Ok(())
    }

    fn drain_pre_quiesce_commands(&mut self) {
        while let Ok(command) = self.data_rx.try_recv() {
            if self.handle_data_command(command) {
                break;
            }
            if self.active_submission.is_some() {
                break;
            }
        }
    }

    fn apply_pending_controls(&mut self) {
        let (resize, nudge, terminal_responses, focus) = {
            let mut controls = self
                .controls
                .lock()
                .unwrap_or_else(|poisoned| poisoned.into_inner());
            (
                controls.resize.take(),
                controls.nudge.take(),
                std::mem::take(&mut controls.terminal_responses),
                controls.focus.take(),
            )
        };
        if self.state == ActorState::Released {
            return;
        }
        if let Some(request) = resize {
            self.resize(request.resize);
            self.enqueue_terminal_responses(request.terminal_responses);
        }
        if let Some(nudge) = nudge {
            self.nudge(nudge);
        }
        self.enqueue_terminal_responses(terminal_responses);
        if let Some(bytes) = focus {
            // A partially written focus sequence is immutable; only the next state coalesces.
            let current_write_offset = self.current_write_offset;
            let pending = self
                .pending_writes
                .iter_mut()
                .enumerate()
                .find(|(index, write)| write.focus && (*index != 0 || current_write_offset == 0));
            if let Some((_, write)) = pending {
                write.bytes = bytes;
            } else {
                self.pending_writes.push_back(PendingWrite {
                    bytes,
                    boundary: None,
                    _permit: None,
                    focus: true,
                });
            }
        }
    }

    fn read_once(&mut self) -> bool {
        let mut buf = [0u8; 8192];
        match self.file.as_ref().read(&mut buf) {
            Ok(0) => false,
            Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => true,
            Err(err) if err.kind() == std::io::ErrorKind::Interrupted => true,
            Err(err) => {
                self.failure.fail(format!("PTY reader failed: {err}"));
                debug!(pane = self.pane_id, err = %err, "PTY actor read failed");
                false
            }
            Ok(n) => {
                let response_order = Arc::clone(&self.response_order);
                let _order = response_order
                    .lock()
                    .unwrap_or_else(|poisoned| poisoned.into_inner());
                let result = (self.on_read)(&buf[..n]);
                if let Some(reason) = result.failure {
                    self.failure.fail(reason);
                    return false;
                }
                if !append_control_responses(
                    &mut self.controls.lock().unwrap_or_else(|p| p.into_inner()),
                    result.terminal_responses,
                ) {
                    self.failure.fail("PTY callback reply budget exhausted");
                    return false;
                }
                drop(_order);
                let terminal_responses = std::mem::take(
                    &mut self
                        .controls
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .terminal_responses,
                );
                self.enqueue_terminal_responses(terminal_responses);
                true
            }
        }
    }

    fn enqueue_terminal_responses(&mut self, terminal_responses: Vec<Bytes>) {
        if self.state == ActorState::Released {
            return;
        }
        for bytes in terminal_responses {
            self.enqueue_write(bytes);
        }
    }

    fn complete_submission_boundary(&mut self, boundary: SubmissionBoundary) {
        match boundary {
            SubmissionBoundary::Text => {
                let Some(submission) = self.active_submission.as_mut() else {
                    return;
                };
                debug_assert!(matches!(submission.phase, SubmissionPhase::WritingText));
                submission.phase = SubmissionPhase::WaitingUntil(Instant::now() + submission.delay);
            }
            SubmissionBoundary::Enter => {
                let Some(submission) = self.active_submission.take() else {
                    return;
                };
                debug_assert!(matches!(submission.phase, SubmissionPhase::WritingEnter));
                let _ = submission.reply.send(Ok(()));
            }
        }
    }

    fn schedule_submission_enter(&mut self) {
        let Some(ActiveSubmission {
            enter,
            phase: SubmissionPhase::WaitingUntil(deadline),
            ..
        }) = self.active_submission.as_ref()
        else {
            return;
        };
        if Instant::now() >= *deadline {
            let enter = enter.clone();
            if enter.is_empty() {
                let submission = self.active_submission.take().unwrap();
                let _ = submission.reply.send(Ok(()));
            } else {
                self.active_submission.as_mut().unwrap().phase = SubmissionPhase::WritingEnter;
                self.enqueue_submission_write(enter, SubmissionBoundary::Enter);
            }
        }
    }

    fn poll_timeout_ms(&self) -> i32 {
        let Some(ActiveSubmission {
            phase: SubmissionPhase::WaitingUntil(deadline),
            ..
        }) = self.active_submission.as_ref()
        else {
            return ACTOR_IDLE_POLL_MS;
        };
        deadline
            .saturating_duration_since(Instant::now())
            .as_millis()
            .max(1)
            .min(ACTOR_IDLE_POLL_MS as u128) as i32
    }

    fn fail_active_submission(&mut self, err: std::io::Error) {
        if let Some(submission) = self.active_submission.take() {
            let _ = submission.reply.send(Err(err));
        }
    }

    fn close_input_queue(&mut self) {
        self.data_rx.close();
        self.fail_active_submission(input_submission_closed_error());
        while let Some(command) = self.data_rx.blocking_recv() {
            if let PtyIoDataCommand::SubmitUserInput { reply, .. } = command {
                let _ = reply.send(Err(input_submission_closed_error()));
            }
        }
    }

    fn flush_pending_writes_once(&mut self) -> std::io::Result<Option<SubmissionBoundary>> {
        while let Some(write) = self.pending_writes.front() {
            let chunk = &write.bytes[self.current_write_offset..];
            match self.file.as_ref().write(chunk) {
                Ok(0) => {
                    return Err(std::io::Error::new(
                        std::io::ErrorKind::WriteZero,
                        "PTY actor write returned zero bytes",
                    ));
                }
                Ok(written) => {
                    self.current_write_offset += written;
                    if self.current_write_offset >= write.bytes.len() {
                        let completed = self.pending_writes.pop_front().unwrap();
                        self.current_write_offset = 0;
                        if let Some(boundary) = completed.boundary {
                            self.file.as_ref().flush()?;
                            return Ok(Some(boundary));
                        }
                    }
                }
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => return Ok(None),
                Err(err) if err.kind() == std::io::ErrorKind::Interrupted => return Ok(None),
                Err(err) => {
                    warn!(pane = self.pane_id, err = %err, "PTY actor write failed");
                    self.pending_writes.clear();
                    self.current_write_offset = 0;
                    return Err(err);
                }
            }
        }
        self.file.as_ref().flush()?;
        Ok(None)
    }

    fn resize(&self, resize: PtyResize) {
        self.log_resize_result(fd::resize_pty_fd(
            self.file.as_raw_fd(),
            resize.rows,
            resize.cols,
            resize.cell_width_px,
            resize.cell_height_px,
        ));
    }

    fn nudge(&mut self, resize: PtyResize) {
        if self.state == ActorState::Released {
            return;
        }
        let nudge = if resize.rows > 2 {
            (
                resize.rows - 1,
                resize.cols,
                resize.cell_width_px,
                resize.cell_height_px,
            )
        } else {
            (
                resize.rows,
                resize.cols.saturating_sub(1).max(4),
                resize.cell_width_px,
                resize.cell_height_px,
            )
        };
        if nudge
            == (
                resize.rows,
                resize.cols,
                resize.cell_width_px,
                resize.cell_height_px,
            )
        {
            return;
        }
        self.log_resize_result(fd::resize_pty_fd(
            self.file.as_raw_fd(),
            nudge.0,
            nudge.1,
            nudge.2,
            nudge.3,
        ));
        std::thread::sleep(Duration::from_millis(30));
        self.log_resize_result(fd::resize_pty_fd(
            self.file.as_raw_fd(),
            resize.rows,
            resize.cols,
            resize.cell_width_px,
            resize.cell_height_px,
        ));
    }

    fn log_resize_result(&self, result: std::io::Result<()>) {
        if let Err(err) = result {
            debug!(pane = self.pane_id, err = %err, "PTY resize failed");
        }
    }
}

fn control_send_error(error: std_mpsc::TrySendError<PtyIoControlCommand>) -> std::io::Error {
    match error {
        std_mpsc::TrySendError::Full(_) => {
            std::io::Error::new(std::io::ErrorKind::WouldBlock, "PTY control queue full")
        }
        std_mpsc::TrySendError::Disconnected(_) => {
            std::io::Error::new(std::io::ErrorKind::BrokenPipe, "PTY actor closed")
        }
    }
}

fn input_submission_closed_error() -> std::io::Error {
    std::io::Error::new(
        std::io::ErrorKind::BrokenPipe,
        "PTY actor closed during input submission",
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        io::{Read, Write},
        os::fd::{AsRawFd, FromRawFd, IntoRawFd},
        os::unix::net::UnixStream,
        sync::atomic::{AtomicBool, Ordering},
    };

    fn test_wake_pair() -> (fd::WakeWriter, OwnedFd) {
        let pipe = fd::create_wake_pipe().expect("wake pipe");
        (pipe.writer, pipe.read_fd)
    }

    fn actor_with_socket_pair(
        initially_quiesced: bool,
    ) -> (PtyIoActorHandle, UnixStream, std_mpsc::Receiver<Bytes>) {
        actor_with_socket_pair_and_poll_observer(initially_quiesced, None)
    }

    #[test]
    fn m5_unix_pty_accepts_full_decoded_paste_plus_markers() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);
        peer.set_read_timeout(Some(Duration::from_secs(10)))
            .expect("bounded PTY read timeout");
        let max = budget::MAX_USER_WRITE_BYTES;
        assert_eq!(
            max,
            crate::protocol::MAX_INPUT_PAYLOAD + budget::PASTE_MARKER_BYTES
        );
        handle
            .try_write_user_input(Bytes::from(vec![b'x'; max]))
            .expect("full admitted bracketed paste fits actor budget");
        let mut received = vec![0; max];
        peer.read_exact(&mut received)
            .expect("actor forwards all admitted PTY bytes");
        assert!(received.iter().all(|byte| *byte == b'x'));
        assert!(matches!(
            handle.try_write_user_input(Bytes::from(vec![b'x'; max + 1])),
            Err(mpsc::error::TrySendError::Full(_))
        ));
        handle.shutdown();
    }

    fn actor_with_socket_pair_and_poll_observer(
        initially_quiesced: bool,
        poll_observer: Option<std_mpsc::Sender<()>>,
    ) -> (PtyIoActorHandle, UnixStream, std_mpsc::Receiver<Bytes>) {
        let (actor_socket, peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("peer timeout");
        let owned = unsafe { OwnedFd::from_raw_fd(actor_socket.into_raw_fd()) };
        let (read_tx, read_rx) = std_mpsc::channel();
        let config = PtyIoActorConfig {
            pane_id: 1,
            master_fd: owned,
            initially_quiesced,
            on_read: Box::new(move |bytes| {
                read_tx
                    .send(Bytes::copy_from_slice(bytes))
                    .expect("read callback receiver alive");
                PtyReadResult::empty()
            }),
            on_reader_exit: None,
        };
        let handle = if let Some(poll_observer) = poll_observer {
            PtyIoActor::spawn_with_poll_observer(config, poll_observer)
        } else {
            PtyIoActor::spawn(config)
        }
        .expect("actor spawn");
        (handle, peer, read_rx)
    }

    #[test]
    fn m1_unix_reply_pressure_fails_instead_of_growing_pending_writes() {
        let (mut runner, _peer) = actor_runner_for_unit_test();
        for _ in 0..64 {
            runner.enqueue_write(Bytes::from_static(b"reply"));
        }
        assert_eq!(runner.pending_writes.len(), 64);
        runner.enqueue_write(Bytes::from_static(b"overflow"));
        assert_eq!(runner.pending_writes.len(), 64);
        assert_eq!(
            runner.failure.reason().as_deref(),
            Some("PTY reply budget exhausted")
        );
    }

    #[test]
    fn m1_unix_control_replies_preserve_order_and_enforce_bytes() {
        let mut controls = SharedPtyControls::default();
        assert!(append_control_responses(
            &mut controls,
            vec![Bytes::from_static(b"first")]
        ));
        assert!(append_control_responses(
            &mut controls,
            vec![Bytes::from_static(b"second")]
        ));
        assert!(!append_control_responses(
            &mut controls,
            vec![Bytes::from(vec![0; 64 * 1024 + 1])]
        ));
        assert_eq!(
            controls.terminal_responses,
            vec![Bytes::from_static(b"first"), Bytes::from_static(b"second")]
        );
    }

    fn actor_runner_for_unit_test() -> (PtyIoActorRunner, UnixStream) {
        let (actor_socket, peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        let owned = unsafe { OwnedFd::from_raw_fd(actor_socket.into_raw_fd()) };
        let (_data_tx, data_rx) = mpsc::channel(ACTOR_COMMAND_BUFFER);
        let (_control_tx, control_rx) = std_mpsc::sync_channel(16);
        let wake_pipe = fd::create_wake_pipe().expect("wake pipe");
        let runner = PtyIoActorRunner {
            _session_admission: None,
            reply_budget: budget::reply_budget(),
            failure: IoFailure::default(),
            stopped: Arc::new(AtomicBool::new(false)),
            pane_id: 1,
            file: Arc::new(std::fs::File::from(owned)),
            data_rx,
            control_rx,
            state: ActorState::Running,
            pending_writes: VecDeque::new(),
            current_write_offset: 0,
            active_submission: None,
            pending_handoff: None,
            wake_read_fd: wake_pipe.read_fd,
            controls: Arc::new(Mutex::new(SharedPtyControls::default())),
            response_order: Arc::new(Mutex::new(())),
            on_read: Box::new(|_| PtyReadResult::empty()),
            on_reader_exit: None,
            poll_observer: None,
        };
        (runner, peer)
    }

    #[test]
    fn actor_ignores_empty_user_input_write() {
        let (mut runner, _peer) = actor_runner_for_unit_test();

        assert!(!runner.handle_data_command(PtyIoDataCommand::WriteUserInput(Bytes::new(), None)));

        assert!(runner.pending_writes.is_empty());
    }

    #[test]
    fn submission_boundary_does_not_wait_for_following_protocol_write() {
        let (mut runner, _peer) = actor_runner_for_unit_test();
        runner.enqueue_submission_write(Bytes::from_static(b"prompt"), SubmissionBoundary::Text);
        runner.enqueue_write(Bytes::from_static(b"response"));

        assert_eq!(
            runner.flush_pending_writes_once().unwrap(),
            Some(SubmissionBoundary::Text)
        );
        assert_eq!(
            runner.pending_writes[0].bytes,
            Bytes::from_static(b"response")
        );
    }

    #[test]
    fn actor_writes_user_input_to_owned_fd() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);

        handle
            .try_write_user_input(Bytes::from_static(b"hello"))
            .expect("write command accepted");

        let mut buf = [0u8; 5];
        peer.read_exact(&mut buf).expect("peer receives write");
        assert_eq!(&buf, b"hello");
        handle.shutdown();
    }

    #[test]
    fn actor_delays_enter_from_completed_prompt_write() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);
        let text = Bytes::from(vec![b'x'; 256 * 1024]);
        let text_len = text.len();
        let delay = Duration::from_millis(200);
        let reader = std::thread::spawn(move || {
            std::thread::sleep(delay);
            let mut received = vec![0; text_len];
            peer.read_exact(&mut received)
                .expect("peer receives prompt");
            let prompt_completed = Instant::now();
            let mut enter = [0; 1];
            peer.read_exact(&mut enter).expect("peer receives enter");
            let enter_received = Instant::now();
            let mut user = [0; 4];
            peer.read_exact(&mut user)
                .expect("peer receives queued input");
            (prompt_completed, enter_received, enter, user)
        });

        let completion = handle
            .queue_user_input_submission(text, Bytes::from_static(b"\r"), delay)
            .expect("submission queues");
        handle
            .try_write_user_input(Bytes::from_static(b"user"))
            .expect("ordinary input queues behind submission");
        completion
            .recv()
            .expect("actor reports submission")
            .expect("submission completes");
        let (prompt_completed, enter_received, enter, user) = reader.join().expect("reader joins");

        assert_eq!(enter, *b"\r");
        assert_eq!(user, *b"user");
        assert!(enter_received.duration_since(prompt_completed) >= delay / 2);

        let err = match handle.queue_user_input_submission(
            Bytes::from_static(b"prompt"),
            Bytes::from_static(b"\r"),
            Duration::ZERO,
        ) {
            Ok(completion) => completion
                .recv()
                .expect("actor reports submission")
                .expect_err("closed PTY rejects submission"),
            Err(err) => err,
        };

        assert!(matches!(
            err.kind(),
            std::io::ErrorKind::BrokenPipe
                | std::io::ErrorKind::ConnectionReset
                | std::io::ErrorKind::WriteZero
        ));
    }

    #[test]
    fn actor_completes_empty_submission_parts() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("peer timeout");

        let completion = handle
            .queue_user_input_submission(Bytes::new(), Bytes::from_static(b"\r"), Duration::ZERO)
            .expect("empty prompt submission queues");
        let mut enter = [0; 1];
        peer.read_exact(&mut enter)
            .expect("peer receives enter for empty prompt");
        assert_eq!(enter, *b"\r");
        completion
            .recv_timeout(Duration::from_secs(1))
            .expect("actor reports empty prompt submission")
            .expect("empty prompt submission completes");

        let completion = handle
            .queue_user_input_submission(
                Bytes::from_static(b"prompt"),
                Bytes::new(),
                Duration::from_millis(40),
            )
            .expect("empty enter submission queues");
        let handoff_handle = handle.clone();
        let handoff =
            std::thread::spawn(move || handoff_handle.begin_handoff(Duration::from_millis(250)));
        let mut prompt = [0; 6];
        peer.read_exact(&mut prompt)
            .expect("peer receives prompt before empty enter");
        assert_eq!(&prompt, b"prompt");
        completion
            .recv_timeout(Duration::from_secs(1))
            .expect("actor reports empty enter submission")
            .expect("empty enter submission completes");
        handoff
            .join()
            .expect("handoff thread joins")
            .expect("handoff resumes without an idle poll after submission");
        handle.shutdown();
    }

    #[test]
    fn actor_reports_peer_closure_during_submission_delay() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);
        let completion = handle
            .queue_user_input_submission(
                Bytes::from_static(b"prompt"),
                Bytes::from_static(b"\r"),
                Duration::from_secs(1),
            )
            .expect("submission queues");
        let mut prompt = [0; 6];
        peer.read_exact(&mut prompt).expect("peer receives prompt");
        drop(peer);

        let err = completion
            .recv_timeout(Duration::from_secs(1))
            .expect("actor reports peer closure")
            .expect_err("peer closure fails the active submission");
        assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn actor_fails_buffered_submissions_on_exit() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);
        let active = handle
            .queue_user_input_submission(
                Bytes::from_static(b"first"),
                Bytes::from_static(b"\r"),
                Duration::from_secs(1),
            )
            .expect("first submission queues");
        let mut prompt = [0; 5];
        peer.read_exact(&mut prompt).expect("peer receives prompt");
        let buffered = handle
            .queue_user_input_submission(
                Bytes::from_static(b"second"),
                Bytes::from_static(b"\r"),
                Duration::ZERO,
            )
            .expect("second submission queues");

        drop(peer);
        let active_err = active
            .recv_timeout(Duration::from_secs(1))
            .expect("actor reports active submission")
            .expect_err("peer closure fails active submission");
        let buffered_err = buffered
            .recv_timeout(Duration::from_secs(1))
            .expect("actor reports buffered submission")
            .expect_err("peer closure fails buffered submission");

        assert_eq!(active_err.kind(), std::io::ErrorKind::BrokenPipe);
        assert_eq!(buffered_err.kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn actor_rejects_submission_after_io_loop_exits() {
        let (actor_socket, peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        let owned = unsafe { OwnedFd::from_raw_fd(actor_socket.into_raw_fd()) };
        let handle_slot = Arc::new(Mutex::new(None::<PtyIoActorHandle>));
        let (attempt_tx, attempt_rx) = std_mpsc::channel();
        let config = PtyIoActorConfig {
            pane_id: 1,
            master_fd: owned,
            initially_quiesced: false,
            on_read: Box::new(|_| PtyReadResult::empty()),
            on_reader_exit: Some(Box::new({
                let handle_slot = Arc::clone(&handle_slot);
                move || {
                    let handle = handle_slot
                        .lock()
                        .unwrap_or_else(|poisoned| poisoned.into_inner())
                        .as_ref()
                        .expect("actor handle installed")
                        .clone();
                    let attempt = handle.queue_user_input_submission(
                        Bytes::from_static(b"prompt"),
                        Bytes::from_static(b"\r"),
                        Duration::ZERO,
                    );
                    attempt_tx.send(attempt).expect("attempt receiver alive");
                }
            })),
        };
        let handle = PtyIoActor::spawn(config).expect("actor spawn");
        *handle_slot
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner()) = Some(handle);

        drop(peer);
        let err = match attempt_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("reader exit callback attempts submission")
        {
            Ok(completion) => completion
                .recv_timeout(Duration::from_secs(1))
                .expect("actor reports submission")
                .expect_err("closed actor rejects submission"),
            Err(err) => err,
        };

        assert_eq!(err.kind(), std::io::ErrorKind::BrokenPipe);
    }

    #[test]
    fn actor_wakes_idle_poll_for_user_input() {
        let (poll_tx, poll_rx) = std_mpsc::channel();
        let (handle, mut peer, _read_rx) =
            actor_with_socket_pair_and_poll_observer(false, Some(poll_tx));
        peer.set_read_timeout(Some(Duration::from_millis(500)))
            .expect("peer timeout");
        poll_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor entered idle poll");

        let start = Instant::now();
        handle
            .try_write_user_input(Bytes::from_static(b"x"))
            .expect("write command accepted");

        let mut buf = [0u8; 1];
        peer.read_exact(&mut buf)
            .expect("peer receives write without waiting for actor poll timeout");
        assert_eq!(&buf, b"x");
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "actor write should be driven by wake fd, not the idle poll timeout"
        );
        handle.shutdown();
    }

    #[test]
    fn actor_reads_output_while_input_is_backpressured() {
        let (mut actor_socket, mut peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("peer timeout");

        let fill = [0xAA; 8192];
        let mut prefilled = 0;
        loop {
            match actor_socket.write(&fill) {
                Ok(written) => prefilled += written,
                Err(err) if err.kind() == std::io::ErrorKind::WouldBlock => break,
                Err(err) => panic!("failed to fill actor write buffer: {err}"),
            }
        }
        assert!(prefilled > 0, "actor write buffer should accept some bytes");

        let owned = unsafe { OwnedFd::from_raw_fd(actor_socket.into_raw_fd()) };
        let (read_tx, read_rx) = std_mpsc::channel();
        let handle = PtyIoActor::spawn(PtyIoActorConfig {
            pane_id: 1,
            master_fd: owned,
            initially_quiesced: false,
            on_read: Box::new(move |bytes| {
                read_tx
                    .send(Bytes::copy_from_slice(bytes))
                    .expect("read callback receiver alive");
                PtyReadResult::empty()
            }),
            on_reader_exit: None,
        })
        .expect("actor spawn");

        let marker = Bytes::from_static(b"queued-input");
        let completion = handle
            .queue_user_input_submission(marker.clone(), Bytes::from_static(b"\r"), Duration::ZERO)
            .expect("submission accepted");

        const OUTPUT_LEN: usize = 128 * 1024;
        let mut peer_writer = peer.try_clone().expect("clone peer writer");
        let output_writer = std::thread::spawn(move || {
            peer_writer
                .write_all(&vec![0xBB; OUTPUT_LEN])
                .expect("peer writes sustained output");
        });
        let deadline = Instant::now() + Duration::from_millis(500);
        let mut output_len = 0;
        while output_len < OUTPUT_LEN {
            let remaining = deadline.saturating_duration_since(Instant::now());
            assert!(
                !remaining.is_zero(),
                "actor did not keep reading blocked peer output"
            );
            let output = read_rx
                .recv_timeout(remaining)
                .expect("actor keeps reading while input remains blocked");
            assert!(output.iter().all(|byte| *byte == 0xBB));
            output_len += output.len();
        }
        assert_eq!(output_len, OUTPUT_LEN);
        output_writer.join().expect("output writer joins");

        let handoff_handle = handle.clone();
        let handoff =
            std::thread::spawn(move || handoff_handle.begin_handoff(Duration::from_secs(1)));

        let mut received_input = vec![0; prefilled + marker.len() + 1];
        peer.read_exact(&mut received_input)
            .expect("peer receives prefill and queued input");
        assert!(received_input[..prefilled].iter().all(|byte| *byte == 0xAA));
        assert_eq!(
            &received_input[prefilled..prefilled + marker.len()],
            marker.as_ref()
        );
        assert_eq!(received_input.last(), Some(&b'\r'));
        completion
            .recv_timeout(Duration::from_secs(1))
            .expect("actor reports submission")
            .expect("submission completes");
        handoff
            .join()
            .expect("handoff thread joins")
            .expect("handoff waits for submission");
        handle.shutdown();
    }

    #[test]
    fn actor_wakes_idle_poll_for_handoff_control() {
        let (poll_tx, poll_rx) = std_mpsc::channel();
        let (handle, _peer, _read_rx) =
            actor_with_socket_pair_and_poll_observer(false, Some(poll_tx));
        poll_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor entered idle poll");

        let start = Instant::now();
        let handoff_handle = handle.clone();
        let handoff =
            std::thread::spawn(move || handoff_handle.begin_handoff(Duration::from_secs(1)));

        handoff
            .join()
            .expect("handoff thread joins")
            .expect("handoff control should wake idle actor");
        assert!(
            start.elapsed() < Duration::from_millis(500),
            "handoff control should be driven by wake fd, not the idle poll timeout"
        );
        handle.shutdown();
    }

    #[test]
    fn poll_ignores_pty_hup_without_pty_interest() {
        let (actor_socket, peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        drop(peer);
        let wake_pipe = fd::create_wake_pipe().expect("wake pipe");

        let readiness = fd::poll_pty_and_wake(
            actor_socket.as_raw_fd(),
            wake_pipe.read_fd.as_raw_fd(),
            false,
            false,
            10,
        )
        .expect("poll succeeds");

        assert!(!readiness.pty_read_ready);
        assert!(!readiness.pty_write_ready);
        assert!(!readiness.wake_ready);
    }

    #[test]
    fn actor_delivers_fd_reads_to_callback() {
        let (handle, mut peer, read_rx) = actor_with_socket_pair(false);

        peer.write_all(b"from-peer").expect("peer write");

        let read = read_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor read callback");
        assert_eq!(read, Bytes::from_static(b"from-peer"));
        handle.shutdown();
    }

    #[test]
    fn begin_handoff_stops_reads_and_rejects_user_writes_until_rollback() {
        let (handle, mut peer, read_rx) = actor_with_socket_pair(false);

        handle
            .begin_handoff(Duration::from_secs(1))
            .expect("handoff quiesced");
        let err = handle
            .begin_handoff(Duration::from_secs(1))
            .expect_err("concurrent handoff rejected");
        assert_eq!(err.kind(), std::io::ErrorKind::WouldBlock);
        assert!(handle
            .try_write_user_input(Bytes::from_static(b"blocked"))
            .is_err());

        peer.write_all(b"held").expect("peer write during quiesce");
        assert!(
            read_rx.recv_timeout(Duration::from_millis(150)).is_err(),
            "actor must not read while quiesced"
        );

        handle.rollback_handoff().expect("rollback resumes actor");
        let read = read_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor reads held bytes after rollback");
        assert_eq!(read, Bytes::from_static(b"held"));

        handle
            .try_write_user_input(Bytes::from_static(b"after"))
            .expect("write accepted after rollback");
        let mut buf = [0u8; 5];
        peer.read_exact(&mut buf).expect("peer receives after");
        assert_eq!(&buf, b"after");
        handle.shutdown();
    }

    #[test]
    fn duplicate_for_handoff_requires_quiesced_actor() {
        let (handle, mut peer, read_rx) = actor_with_socket_pair(false);

        assert!(handle.duplicate_for_handoff().is_err());
        handle
            .begin_handoff(Duration::from_secs(1))
            .expect("handoff quiesced");
        let duplicate = handle
            .duplicate_for_handoff()
            .expect("handoff duplicate created");
        assert!(duplicate >= 0);
        unsafe {
            libc::close(duplicate);
        }
        handle.rollback_handoff().expect("rollback resumes actor");

        peer.write_all(b"still-live").expect("peer write");
        let read = read_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("actor still reads after duplicate closes");
        assert_eq!(read, Bytes::from_static(b"still-live"));
        handle.shutdown();
    }

    #[tokio::test]
    async fn input_preflight_preserves_handoff_and_shutdown_write_gates() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);
        let runtime = crate::terminal::TerminalRuntime::test_with_actor(handle.clone());
        handle.begin_handoff(Duration::from_secs(1)).unwrap();
        // A successful preflight is not permission to bypass the existing gate.
        runtime.validate_input_submission(b"blocked").unwrap();
        assert!(runtime
            .try_send_bytes(Bytes::from_static(b"blocked"))
            .is_err());
        handle.rollback_handoff().unwrap();
        runtime.validate_input_submission(b"after").unwrap();
        runtime
            .try_send_bytes(Bytes::from_static(b"after"))
            .unwrap();
        let mut bytes = [0; 5];
        peer.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"after");
        handle.shutdown();
        assert!(runtime
            .try_send_bytes(Bytes::from_static(b"closed"))
            .is_err());
    }

    #[test]
    fn resize_and_nudge_keep_latest_request_when_command_queue_is_full() {
        let (data_tx, _data_rx) = mpsc::channel(1);
        let (control_tx, _control_rx) = std_mpsc::sync_channel(16);
        data_tx
            .try_send(PtyIoDataCommand::WriteUserInput(
                Bytes::from_static(b"fill"),
                None,
            ))
            .expect("fill command queue");
        let controls = Arc::new(Mutex::new(SharedPtyControls::default()));
        let (wake, _wake_read_fd) = test_wake_pair();
        let handle = PtyIoActorHandle {
            input_budget: budget::input_budget(),
            failure: IoFailure::default(),
            stopped: Arc::new(AtomicBool::new(false)),
            data_tx,
            control_tx,
            wake,
            user_writes: Arc::new(Mutex::new(UserWriteGate { accepting: true })),
            controls: Arc::clone(&controls),
            response_order: Arc::new(Mutex::new(())),
            input_guard: Default::default(),
        };

        handle.resize(20, 80, 8, 16, vec![Bytes::from_static(b"old")]);
        handle.resize(40, 120, 9, 18, vec![Bytes::from_static(b"new")]);
        handle.nudge_child_redraw_after_handoff(41, 121, 10, 20);
        handle.write_terminal_response(|| Some(Bytes::from_static(b"response")));

        let controls = controls.lock().expect("controls lock");
        assert_eq!(
            controls.resize,
            Some(PtyResizeRequest {
                resize: PtyResize {
                    rows: 40,
                    cols: 120,
                    cell_width_px: 9,
                    cell_height_px: 18,
                },
                terminal_responses: Vec::new(),
            })
        );
        assert_eq!(
            controls.nudge,
            Some(PtyResize {
                rows: 41,
                cols: 121,
                cell_width_px: 10,
                cell_height_px: 20,
            })
        );
        assert_eq!(
            controls.terminal_responses,
            vec![
                Bytes::from_static(b"old"),
                Bytes::from_static(b"new"),
                Bytes::from_static(b"response")
            ]
        );
    }

    #[test]
    fn appearance_transition_report_precedes_query_of_new_scheme() {
        let (actor_socket, mut peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        let owned = unsafe { OwnedFd::from_raw_fd(actor_socket.into_raw_fd()) };
        let (data_tx, data_rx) = mpsc::channel(ACTOR_COMMAND_BUFFER);
        let (control_tx, control_rx) = std_mpsc::sync_channel(16);
        let wake_pipe = fd::create_wake_pipe().expect("wake pipe");
        let controls = Arc::new(Mutex::new(SharedPtyControls::default()));
        let response_order = Arc::new(Mutex::new(()));
        let light = Arc::new(AtomicBool::new(false));
        let query_light = Arc::clone(&light);
        let runner = PtyIoActorRunner {
            _session_admission: None,
            reply_budget: budget::reply_budget(),
            failure: IoFailure::default(),
            stopped: Arc::new(AtomicBool::new(false)),
            pane_id: 1,
            file: Arc::new(std::fs::File::from(owned)),
            data_rx,
            control_rx,
            state: ActorState::Running,
            pending_writes: VecDeque::new(),
            current_write_offset: 0,
            active_submission: None,
            pending_handoff: None,
            wake_read_fd: wake_pipe.read_fd,
            controls: Arc::clone(&controls),
            response_order: Arc::clone(&response_order),
            on_read: Box::new(move |_| PtyReadResult {
                failure: None,
                terminal_responses: vec![if query_light.load(Ordering::Acquire) {
                    Bytes::from_static(b"query-light")
                } else {
                    Bytes::from_static(b"query-dark")
                }],
            }),
            on_reader_exit: None,
            poll_observer: None,
        };
        let handle = PtyIoActorHandle {
            input_budget: budget::input_budget(),
            failure: IoFailure::default(),
            stopped: Arc::new(AtomicBool::new(false)),
            data_tx,
            control_tx,
            wake: wake_pipe.writer,
            user_writes: Arc::new(Mutex::new(UserWriteGate { accepting: true })),
            controls,
            response_order,
            input_guard: crate::platform::PtyInputGuard::new(&runner.file),
        };
        let (changed_tx, changed_rx) = std_mpsc::channel();
        let (continue_tx, continue_rx) = std_mpsc::channel();

        let appearance = std::thread::spawn(move || {
            handle.write_terminal_response(|| {
                light.store(true, Ordering::Release);
                changed_tx.send(()).expect("notify appearance change");
                continue_rx.recv().expect("continue appearance report");
                Some(Bytes::from_static(b"live-light"))
            });
        });
        changed_rx.recv().expect("appearance changed");
        peer.write_all(b"query").expect("write query");
        let reader = std::thread::spawn(move || {
            let mut runner = runner;
            assert!(runner.read_once());
            runner
        });
        continue_tx.send(()).expect("release appearance report");
        appearance.join().expect("appearance thread joins");
        let runner = reader.join().expect("reader thread joins");

        assert_eq!(
            runner.pending_writes,
            VecDeque::from([
                PendingWrite {
                    focus: false,
                    _permit: None,
                    bytes: Bytes::from_static(b"live-light"),
                    boundary: None,
                },
                PendingWrite {
                    focus: false,
                    _permit: None,
                    bytes: Bytes::from_static(b"query-light"),
                    boundary: None,
                },
            ])
        );
    }

    #[test]
    fn resize_writes_terminal_responses_after_applying_resize() {
        let (handle, mut peer, _read_rx) = actor_with_socket_pair(false);
        let response = Bytes::from_static(b"\x1B[48;40;100;720;900t");

        handle.resize(40, 100, 9, 18, vec![response.clone()]);

        let mut buf = vec![0; response.len()];
        peer.read_exact(&mut buf)
            .expect("peer receives resize response");
        assert_eq!(Bytes::from(buf), response);
        handle.shutdown();
    }

    #[test]
    fn handoff_control_is_not_blocked_by_full_data_queue() {
        let (data_tx, _data_rx) = mpsc::channel(1);
        let (control_tx, control_rx) = std_mpsc::sync_channel(16);
        data_tx
            .try_send(PtyIoDataCommand::WriteUserInput(
                Bytes::from_static(b"fill"),
                None,
            ))
            .expect("fill data queue");
        let (wake, _wake_read_fd) = test_wake_pair();
        let handle = PtyIoActorHandle {
            input_budget: budget::input_budget(),
            failure: IoFailure::default(),
            stopped: Arc::new(AtomicBool::new(false)),
            data_tx,
            control_tx,
            wake,
            user_writes: Arc::new(Mutex::new(UserWriteGate { accepting: true })),
            controls: Arc::new(Mutex::new(SharedPtyControls::default())),
            response_order: Arc::new(Mutex::new(())),
            input_guard: Default::default(),
        };

        let handoff = std::thread::spawn(move || handle.begin_handoff(Duration::from_secs(1)));
        match control_rx
            .recv_timeout(Duration::from_secs(1))
            .expect("handoff control command")
        {
            PtyIoControlCommand::BeginHandoff(reply) => {
                reply.send(Ok(())).expect("handoff waiter alive");
            }
            _ => panic!("expected begin handoff command"),
        }

        handoff
            .join()
            .expect("handoff thread joins")
            .expect("handoff succeeds despite full data queue");
    }

    #[test]
    fn begin_handoff_drains_user_writes_already_in_command_queue() {
        let (actor_socket, mut peer) = UnixStream::pair().expect("socket pair");
        actor_socket
            .set_nonblocking(true)
            .expect("actor socket nonblocking");
        peer.set_read_timeout(Some(Duration::from_secs(1)))
            .expect("peer timeout");
        let (data_tx, data_rx) = mpsc::channel(ACTOR_COMMAND_BUFFER);
        let (_control_tx, control_rx) = std_mpsc::channel();
        data_tx
            .try_send(PtyIoDataCommand::WriteUserInput(
                Bytes::from_static(b"queued-before-ack"),
                None,
            ))
            .expect("queued write");
        let mut runner = PtyIoActorRunner {
            _session_admission: None,
            reply_budget: budget::reply_budget(),
            failure: IoFailure::default(),
            stopped: Arc::new(AtomicBool::new(false)),
            pane_id: 1,
            file: Arc::new(std::fs::File::from(unsafe {
                OwnedFd::from_raw_fd(actor_socket.into_raw_fd())
            })),
            data_rx,
            control_rx,
            state: ActorState::Running,
            pending_writes: VecDeque::new(),
            current_write_offset: 0,
            active_submission: None,
            pending_handoff: None,
            wake_read_fd: fd::create_wake_pipe().expect("wake pipe").read_fd,
            controls: Arc::new(Mutex::new(SharedPtyControls::default())),
            response_order: Arc::new(Mutex::new(())),
            on_read: Box::new(|_| PtyReadResult::empty()),
            on_reader_exit: None,
            poll_observer: None,
        };

        runner.begin_handoff().expect("handoff drains queued write");

        let mut buf = [0u8; 17];
        peer.read_exact(&mut buf)
            .expect("queued write reaches peer before quiesce ack");
        assert_eq!(&buf, b"queued-before-ack");
        assert_eq!(runner.state, ActorState::Quiesced);
    }

    #[test]
    fn release_after_commit_prevents_further_io() {
        let (handle, mut peer, read_rx) = actor_with_socket_pair(false);

        handle.release_after_commit().expect("actor released");
        assert!(handle
            .try_write_user_input(Bytes::from_static(b"blocked"))
            .is_err());

        let _ = peer.write_all(b"ignored");
        assert!(read_rx.recv_timeout(Duration::from_millis(150)).is_err());
    }
}
