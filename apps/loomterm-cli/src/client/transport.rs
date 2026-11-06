// Modified for Loomterm (2026-11-05); see apps/loomterm-cli/PATCHES.md.
use super::*;
use crate::pty::budget::{Budget, Permit};
use std::io::Read as _;
use std::sync::Mutex;
use std::time::Instant;
mod frame_admission;
use frame_admission::MAX_FRAME_BYTES;

fn frame_budget() -> Arc<Budget> {
    static BUDGET: std::sync::OnceLock<Arc<Budget>> = std::sync::OnceLock::new();
    BUDGET
        .get_or_init(|| Budget::new(4, MAX_FRAME_BYTES))
        .clone()
}

fn wait_frame_admission(
    budget: &Arc<Budget>,
    stopped: &AtomicBool,
    closed: impl Fn() -> bool,
    deadline: Instant,
) -> io::Result<Permit> {
    loop {
        if stopped.load(Ordering::Acquire) || closed() {
            return Err(io::Error::new(
                io::ErrorKind::ConnectionAborted,
                "client frame reader stopped",
            ));
        }
        if Instant::now() >= deadline {
            return Err(io::Error::new(
                io::ErrorKind::TimedOut,
                "client UI frame admission timed out",
            ));
        }
        // Decode may expand a small delta to a whole cell frame. Reserve the entire allowance
        // before allocating, then shrink only after decoding and accounting the owned message.
        if let Some(permit) = budget.acquire(MAX_FRAME_BYTES) {
            return Ok(permit);
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

fn record_reader_failure(
    stopped: &AtomicBool,
    error: &Mutex<Option<io::Error>>,
    failure: io::Error,
) {
    let mut slot = error.lock().unwrap_or_else(|error| error.into_inner());
    // Explicit disconnect already cancels the reader. Preserve the first actual I/O failure.
    if !stopped.load(Ordering::Acquire) {
        slot.get_or_insert(failure);
        stopped.store(true, Ordering::Release);
    }
}

fn send_client_event(
    sender: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
    mut event: ClientLoopEvent,
    stopped: &AtomicBool,
    deadline: Instant,
) -> bool {
    loop {
        match sender.try_send(event) {
            Ok(()) => return true,
            Err(tokio::sync::mpsc::error::TrySendError::Closed(_)) => return false,
            Err(tokio::sync::mpsc::error::TrySendError::Full(pending)) => event = pending,
        }
        if stopped.load(Ordering::Acquire) || Instant::now() >= deadline {
            return false;
        }
        std::thread::sleep(Duration::from_millis(2));
    }
}

struct PrefixedFrame<'a, R> {
    prefix: [u8; 4],
    offset: usize,
    reader: &'a mut R,
}
impl<R: io::Read> io::Read for PrefixedFrame<'_, R> {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if self.offset < self.prefix.len() {
            let count = bytes.len().min(self.prefix.len() - self.offset);
            bytes[..count].copy_from_slice(&self.prefix[self.offset..self.offset + count]);
            self.offset += count;
            Ok(count)
        } else {
            self.reader.read(bytes)
        }
    }
}

pub(super) fn start_endpoint_transport(
    stream: LocalStream,
    lifetime: impl Send + 'static,
    event_tx: &tokio::sync::mpsc::Sender<ClientLoopEvent>,
    endpoint_id: endpoint::ClientEndpointId,
    generation: u64,
    max_frame_size: usize,
    surface_decoder: Option<protocol::surface_reuse::Decoder>,
) -> Result<endpoint::NativeEndpointTransport, ClientError> {
    let reader = stream.try_clone().map_err(ClientError::ConnectionFailed)?;
    let transport = endpoint::NativeEndpointTransport::with_lifetime(stream, lifetime)
        .map_err(ClientError::ConnectionFailed)?;
    let stopped = transport.stop_handle();
    let error = transport.error_handle();
    let event_tx = event_tx.clone();
    std::thread::Builder::new()
        .name("endpoint-reader".into())
        .spawn(move || {
            server_reader_thread(
                reader,
                event_tx,
                &stopped,
                max_frame_size,
                endpoint_id,
                generation,
                surface_decoder,
                &error,
            );
        })
        .map_err(ClientError::ConnectionFailed)?;
    Ok(transport)
}

/// Reads complete frames while retaining partial-read progress across nonblocking polls.
#[allow(clippy::too_many_arguments)]
pub(super) fn server_reader_thread(
    #[allow(unused_mut)] mut stream: LocalStream,
    event_tx: tokio::sync::mpsc::Sender<ClientLoopEvent>,
    should_quit: &Arc<AtomicBool>,
    max_frame_size: usize,
    endpoint_id: endpoint::ClientEndpointId,
    generation: u64,
    mut surface_decoder: Option<protocol::surface_reuse::Decoder>,
    failure: &Mutex<Option<io::Error>>,
) {
    #[cfg(windows)]
    let mut stream = crate::server::client_transport::WindowsStreamGuard::new(stream);
    if let Err(error) = stream.set_nonblocking(true) {
        record_reader_failure(should_quit, failure, error);
        let _ = send_client_event(
            &event_tx,
            ClientLoopEvent::ServerDisconnected {
                endpoint_id,
                generation,
            },
            should_quit,
            Instant::now() + Duration::from_secs(5),
        );
        return;
    }

    let mut stream = EndpointReader {
        stream: &mut stream,
        stopped: should_quit,
        deadline: None,
    };
    loop {
        if should_quit.load(Ordering::Acquire) {
            break;
        }

        let message = (|| -> Result<(ServerMessage, Permit), protocol::FramingError> {
            // Idle endpoints hold no admission. Only a four-byte prefix is read before reserving.
            stream.deadline = None;
            let mut prefix = [0u8; 4];
            let mut offset = 0;
            while offset < prefix.len() {
                let count = stream.read(&mut prefix[offset..])?;
                if count == 0 {
                    return Err(protocol::FramingError::UnexpectedEof);
                }
                if offset == 0 {
                    stream.deadline = Some(Instant::now() + Duration::from_secs(5));
                }
                offset += count;
            }
            let claimed = u32::from_le_bytes(prefix) as usize;
            if claimed > max_frame_size {
                return Err(protocol::FramingError::Oversized {
                    claimed,
                    max: max_frame_size,
                });
            }
            let deadline = stream.deadline.expect("started frame deadline");
            let mut admission = wait_frame_admission(
                &frame_budget(),
                should_quit,
                || event_tx.is_closed(),
                deadline,
            )?;
            let message = protocol::read_message(
                &mut PrefixedFrame {
                    prefix,
                    offset: 0,
                    reader: &mut stream,
                },
                max_frame_size,
            )?;
            let message = match &mut surface_decoder {
                Some(decoder) => {
                    let message = decoder.decode(message).map_err(|error| {
                        protocol::FramingError::Io(io::Error::new(
                            io::ErrorKind::InvalidData,
                            error,
                        ))
                    })?;
                    if decoder
                        .retained_bytes()
                        .is_none_or(|bytes| bytes > MAX_FRAME_BYTES)
                    {
                        return Err(protocol::FramingError::Io(io::Error::new(
                            io::ErrorKind::InvalidData,
                            "client cell baseline exceeds 64 MiB",
                        )));
                    }
                    message
                }
                None => message,
            };
            let owned_bytes = frame_admission::decoded_bytes(&message).map_err(|error| {
                protocol::FramingError::Io(io::Error::new(io::ErrorKind::InvalidData, error))
            })?;
            admission.shrink_to(owned_bytes);
            Ok((message, admission))
        })();
        match message {
            Ok((msg, admission)) => {
                let server_shutdown = matches!(msg, ServerMessage::ServerShutdown { .. });
                if !send_client_event(
                    &event_tx,
                    ClientLoopEvent::ServerMessage {
                        endpoint_id: endpoint_id.clone(),
                        generation,
                        message: Box::new(msg),
                        _admission: admission,
                    },
                    should_quit,
                    Instant::now() + Duration::from_secs(5),
                ) {
                    record_reader_failure(
                        should_quit,
                        failure,
                        io::Error::new(
                            io::ErrorKind::TimedOut,
                            "client UI did not consume queued frame",
                        ),
                    );
                    break;
                }
                // This terminal message already conveys the close reason. Reading its following
                // EOF could race the UI timer and replace that reason with a transport failure.
                if server_shutdown {
                    break;
                }
            }
            Err(protocol::FramingError::UnexpectedEof) => {
                record_reader_failure(
                    should_quit,
                    failure,
                    io::Error::new(io::ErrorKind::UnexpectedEof, "endpoint reader closed"),
                );
                let _ = send_client_event(
                    &event_tx,
                    ClientLoopEvent::ServerDisconnected {
                        endpoint_id: endpoint_id.clone(),
                        generation,
                    },
                    should_quit,
                    Instant::now() + Duration::from_secs(5),
                );
                break;
            }
            Err(protocol::FramingError::Io(err)) if err.kind() == io::ErrorKind::WouldBlock => {
                std::thread::sleep(Duration::from_millis(1));
            }
            Err(err) => {
                warn!(err = %err, "server read error");
                let error = match err {
                    protocol::FramingError::Io(error) => error,
                    error => io::Error::new(io::ErrorKind::InvalidData, error),
                };
                record_reader_failure(should_quit, failure, error);
                let _ = send_client_event(
                    &event_tx,
                    ClientLoopEvent::ServerDisconnected {
                        endpoint_id: endpoint_id.clone(),
                        generation,
                    },
                    should_quit,
                    Instant::now() + Duration::from_secs(5),
                );
                break;
            }
        }
    }
}

pub(super) struct EndpointReader<'a> {
    pub(super) stream: &'a mut LocalStream,
    pub(super) stopped: &'a AtomicBool,
    pub(super) deadline: Option<Instant>,
}

impl io::Read for EndpointReader<'_> {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.stopped.load(Ordering::Acquire) {
                return Ok(0);
            }
            let remaining = match self.deadline {
                Some(deadline) => {
                    deadline
                        .checked_duration_since(Instant::now())
                        .ok_or_else(|| {
                            io::Error::new(io::ErrorKind::TimedOut, "handshake response timed out")
                        })?
                }
                None => Duration::from_millis(100),
            };
            match crate::ipc::poll_local_stream_read_count(self.stream, buffer)? {
                crate::ipc::LocalStreamReadCount::Data(count) => return Ok(count),
                crate::ipc::LocalStreamReadCount::Closed => return Ok(0),
                crate::ipc::LocalStreamReadCount::Pending => {
                    crate::platform::wait_client_stream_readable(self.stream, remaining)?;
                }
            }
        }
    }
}

pub(in crate::client) fn write_to_local_server(
    stream: &mut LocalStream,
    msg: &ClientMessage,
) -> io::Result<()> {
    protocol::write_message(stream, msg).map_err(|error| io::Error::other(error.to_string()))
}

pub(super) trait ClientMessageSink {
    fn send_client_message(&mut self, message: &ClientMessage) -> io::Result<()>;
}

impl ClientMessageSink for LocalStream {
    fn send_client_message(&mut self, message: &ClientMessage) -> io::Result<()> {
        write_to_local_server(self, message)
    }
}

impl ClientMessageSink for endpoint::EndpointRegistry {
    fn send_client_message(&mut self, message: &ClientMessage) -> io::Result<()> {
        // The lifecycle loop consumes failures for every endpoint, including Local. A send
        // failure must not bypass that transition or tear down unrelated connections.
        self.send(message);
        Ok(())
    }
}

pub(super) fn write_to_server(
    stream: &mut impl ClientMessageSink,
    msg: &ClientMessage,
) -> io::Result<()> {
    stream.send_client_message(msg)
}

#[cfg(all(test, unix))]
mod tests {
    use super::*;
    use crate::client::endpoint::EndpointTransport as _;
    use interprocess::local_socket::traits::Listener as _;
    use std::io::{Read as _, Write as _};
    use std::time::Instant;

    #[test]
    fn upload_cancellation_preserves_pending_endpoint_download() {
        let path = std::env::temp_dir().join(format!(
            "herdr-cancel-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = crate::ipc::bind_private_local_listener(&path).unwrap();
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        let mut bridge = listener.accept().unwrap();
        std::fs::remove_file(path).unwrap();
        drop(listener);
        let mut reader_stream = client.try_clone().unwrap();
        let mut writer = endpoint::NativeEndpointTransport::with_lifetime(client, ()).unwrap();
        let stopped = writer.stop_handle();
        struct ForwardedInput(std::sync::mpsc::Sender<Vec<u8>>);
        impl io::Write for ForwardedInput {
            fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
                self.0.send(bytes.to_vec()).unwrap();
                Ok(bytes.len())
            }

            fn flush(&mut self) -> io::Result<()> {
                Ok(())
            }
        }
        let (forwarded_tx, forwarded_rx) = std::sync::mpsc::channel();
        let cancel = crate::remote::bridge_upload_cancellation_for_test(
            bridge.try_clone().unwrap(),
            ForwardedInput(forwarded_tx),
        );
        let message = ClientMessage::ClientShellFocus { focused: false };
        let mut expected = Vec::new();
        protocol::write_message(&mut expected, &message).unwrap();
        writer.send(&message).unwrap();
        let mut forwarded = Vec::new();
        while forwarded.len() < expected.len() {
            forwarded.extend(forwarded_rx.recv_timeout(Duration::from_secs(3)).unwrap());
        }
        assert_eq!(forwarded, expected);
        cancel();

        // A client write after upload cancellation must not stop the download reader.
        writer
            .send(&ClientMessage::ClientShellFocus { focused: true })
            .unwrap();
        let flushed = writer.flush(Instant::now() + Duration::from_secs(3));
        if flushed.is_ok() {
            let received: ClientMessage =
                protocol::read_message(&mut bridge, protocol::MAX_FRAME_SIZE).unwrap();
            assert_eq!(received, ClientMessage::ClientShellFocus { focused: true });
        }
        const FINAL: &[u8] = b"pending-download: FINAL OUTPUT\n";
        bridge.write_all(FINAL).unwrap();
        drop(bridge);
        let mut output = Vec::new();
        EndpointReader {
            stream: &mut reader_stream,
            stopped: &stopped,
            deadline: None,
        }
        .read_to_end(&mut output)
        .unwrap();
        assert_eq!(output, FINAL);
        assert!(flushed.is_ok(), "client write failed: {flushed:?}");
        assert!(!stopped.load(Ordering::Acquire));
        assert!(writer.take_error().is_none());
    }
}

#[cfg(test)]
mod admission_tests {
    use super::*;

    #[test]
    fn m1_reader_failure_survives_full_ui_queue_and_releases_frame() {
        let budget = Budget::new(4, MAX_FRAME_BYTES);
        let admission = budget.acquire(MAX_FRAME_BYTES).unwrap();
        let stopped = AtomicBool::new(false);
        let failure = Mutex::new(None);
        let (sender, mut receiver) = tokio::sync::mpsc::channel(1);
        assert!(sender.try_send(ClientLoopEvent::Timer).is_ok());
        assert!(!send_client_event(
            &sender,
            ClientLoopEvent::ServerMessage {
                endpoint_id: endpoint::ClientEndpointId::Local,
                generation: 1,
                message: Box::new(ServerMessage::ServerShutdown {
                    reason: Some("test".into()),
                }),
                _admission: admission,
            },
            &stopped,
            Instant::now(),
        ));
        record_reader_failure(
            &stopped,
            &failure,
            io::Error::new(io::ErrorKind::TimedOut, "UI queue stayed full"),
        );
        assert!(stopped.load(Ordering::Acquire));
        assert!(budget.acquire(MAX_FRAME_BYTES).is_some());
        assert!(matches!(receiver.try_recv(), Ok(ClientLoopEvent::Timer)));
        assert!(receiver.is_empty());
        assert_eq!(
            failure.lock().unwrap().as_ref().unwrap().kind(),
            io::ErrorKind::TimedOut
        );
        record_reader_failure(
            &stopped,
            &failure,
            io::Error::new(io::ErrorKind::ConnectionAborted, "later cancellation"),
        );
        assert_eq!(
            failure.lock().unwrap().as_ref().unwrap().kind(),
            io::ErrorKind::TimedOut
        );
        let explicit_stop = Mutex::new(None);
        record_reader_failure(
            &stopped,
            &explicit_stop,
            io::Error::new(io::ErrorKind::UnexpectedEof, "explicitly stopped"),
        );
        assert!(explicit_stop.lock().unwrap().is_none());
    }

    #[test]
    fn m1_frame_admission_wait_is_cancellable_without_releasing_an_owned_frame() {
        let budget = Budget::new(4, MAX_FRAME_BYTES);
        let mut held = budget.acquire(MAX_FRAME_BYTES).unwrap();
        held.shrink_to(1024);
        let cancelled = AtomicBool::new(true);
        assert_eq!(
            wait_frame_admission(
                &budget,
                &cancelled,
                || false,
                Instant::now() + Duration::from_secs(1)
            )
            .unwrap_err()
            .kind(),
            io::ErrorKind::ConnectionAborted
        );
        assert!(budget.acquire(MAX_FRAME_BYTES).is_none());
        drop(held);
        assert!(wait_frame_admission(
            &budget,
            &AtomicBool::new(false),
            || false,
            Instant::now() + Duration::from_secs(1)
        )
        .is_ok());
    }

    #[test]
    fn m1_frame_accounting_charges_expanded_owned_cells() {
        let cells = vec![
            protocol::CellData {
                symbol: String::new(),
                fg: 0,
                bg: 0,
                modifier: 0,
                skip: false,
                hyperlink: None
            };
            1024
        ];
        let bytes = frame_admission::decoded_bytes(&cells).unwrap();
        assert!(bytes >= cells.len() * std::mem::size_of::<protocol::CellData>());
        assert!(
            bytes > 1024,
            "a small wire delta cannot pay only for its changed cells"
        );
    }

    #[cfg(windows)]
    #[test]
    fn m1_slow_ui_retains_admission_through_consume_and_preserves_reuse_order() {
        use interprocess::local_socket::traits::Listener as _;
        struct Stop(Arc<AtomicBool>);
        impl Drop for Stop {
            fn drop(&mut self) {
                self.0.store(true, Ordering::Release);
            }
        }
        let path = std::env::temp_dir().join(format!(
            "loomterm-ui-budget-{}-{}.sock",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        let listener = crate::ipc::bind_local_listener(&path).unwrap();
        let client = crate::ipc::connect_local_stream(&path).unwrap();
        let server = listener.accept().unwrap();
        let stopped = Arc::new(AtomicBool::new(false));
        let stop = Stop(stopped.clone());
        let (sender, mut receiver) = tokio::sync::mpsc::channel(256);
        let reader_stop = stopped.clone();
        let failure = Arc::new(Mutex::new(None));
        let reader_failure = failure.clone();
        let reader = std::thread::spawn(move || {
            server_reader_thread(
                client,
                sender,
                &reader_stop,
                protocol::MAX_FRAME_SIZE,
                endpoint::ClientEndpointId::Local,
                1,
                Some(protocol::surface_reuse::Decoder::new(false, false)),
                &reader_failure,
            )
        });
        let writer = std::thread::spawn(move || {
            let mut server = crate::server::client_transport::WindowsStreamGuard::new(server);
            server.set_nonblocking(true).unwrap();
            let mut surface = protocol::PaneSurfaceFrame {
                boot_id: "budget".into(),
                projection_revision: 1,
                surface_revision: 1,
                frame: protocol::FrameData {
                    cells: vec![
                        protocol::CellData {
                            symbol: "x".into(),
                            fg: 0,
                            bg: 0,
                            modifier: 0,
                            skip: false,
                            hyperlink: None
                        };
                        100
                    ],
                    width: 10,
                    height: 10,
                    cursor: None,
                    hyperlinks: Vec::new(),
                    graphics: Vec::new(),
                },
                panes: Vec::new(),
                splits: Vec::new(),
                popup: None,
                graphics: Default::default(),
            };
            for revision in 1..=3 {
                surface.surface_revision = revision;
                let message = if revision == 1 {
                    ServerMessage::PaneSurface(surface.clone())
                } else {
                    protocol::surface_reuse::message(revision - 1, &mut surface)
                        .unwrap()
                        .unwrap()
                };
                let mut bytes = Vec::new();
                protocol::write_message(&mut bytes, &message).unwrap();
                crate::server::client_transport::write_windows_frame(&mut *server, &bytes, || {
                    false
                })
                .unwrap();
            }
            let mut bytes = Vec::new();
            protocol::write_message(
                &mut bytes,
                &ServerMessage::ServerShutdown {
                    reason: Some("detached".into()),
                },
            )
            .unwrap();
            crate::server::client_transport::write_windows_frame(&mut *server, &bytes, || false)
                .unwrap();
            server.finish_response();
        });
        let deadline = Instant::now() + Duration::from_secs(3);
        let receive = |receiver: &mut tokio::sync::mpsc::Receiver<ClientLoopEvent>| loop {
            match receiver.try_recv() {
                Ok(event) => break event,
                Err(tokio::sync::mpsc::error::TryRecvError::Empty) if Instant::now() < deadline => {
                    std::thread::sleep(Duration::from_millis(2))
                }
                _ => panic!("expected the next admitted surface"),
            }
        };
        for revision in 1..=3 {
            let event = receive(&mut receiver);
            let ClientLoopEvent::ServerMessage {
                message,
                _admission,
                ..
            } = event
            else {
                panic!("expected ordered surface");
            };
            let ServerMessage::PaneSurface(surface) = *message else {
                panic!("reuse must expand to a complete surface");
            };
            assert_eq!(surface.surface_revision, revision);
            assert_eq!(surface.frame.cells.len(), 100);
            std::thread::sleep(Duration::from_millis(20));
            assert!(
                receiver.is_empty(),
                "dequeue alone cannot release frame admission"
            );
            drop(_admission);
        }
        let ClientLoopEvent::ServerMessage { message, .. } = receive(&mut receiver) else {
            panic!("expected the explicit terminal message");
        };
        assert!(matches!(
            *message,
            ServerMessage::ServerShutdown { reason: Some(reason) } if reason == "detached"
        ));
        reader.join().unwrap();
        writer.join().unwrap();
        assert!(!stopped.load(Ordering::Acquire));
        assert!(failure.lock().unwrap().is_none());
        drop(stop);
        drop(listener);
        std::fs::remove_file(path).unwrap();
    }
}
