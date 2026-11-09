//! Opt-in outer terminal host for the Windows and Unix native controllers.
//! Commands drive a real OS PTY; they never synthesize application IPC.
#![cfg(any(windows, unix))]

use std::fs::File;
use std::io::{self, BufRead, Read, Write};
use std::sync::{mpsc, Arc, Mutex};
use std::time::{Duration, Instant};

use ghostty_vt::Terminal;
use portable_pty::{native_pty_system, CommandBuilder, PtySize};
use serde_json::{json, Value};

enum Event {
    Spawned(OwnedChild),
    Command(Value),
    Output(Vec<u8>),
    Closed,
    Error(String),
}

struct OwnedChild(Box<dyn portable_pty::Child + Send + Sync>);

impl OwnedChild {
    fn terminate(&mut self) {
        #[cfg(windows)]
        {
            let _ = self.0.kill();
        }
        #[cfg(unix)]
        if let Some(pid) = self.0.process_id() {
            // SAFETY: this is the unreaped direct child owned by this test host.
            // Request ordinary termination, never a process-name match or SIGKILL.
            unsafe {
                libc::kill(pid as libc::pid_t, libc::SIGTERM);
            }
        }
    }
}

impl Drop for OwnedChild {
    fn drop(&mut self) {
        if !matches!(self.0.try_wait(), Ok(Some(_))) {
            self.terminate();
        }
    }
}

fn emit(value: Value) {
    println!("{value}");
    io::stdout().flush().unwrap();
}

#[test]
#[ignore = "requires the explicitly isolated Windows native test controller"]
#[cfg(windows)]
fn outer_conpty_host() {
    run_outer_host();
}

#[test]
#[ignore = "requires the explicitly isolated Unix native test controller"]
#[cfg(unix)]
fn outer_unix_host() {
    run_outer_host();
}

fn run_outer_host() {
    let root = std::env::var_os("LOOMTERM_ROOT").expect("explicit isolated Root");
    let cwd = std::env::var_os("LOOMTERM_TEST_SOURCE").expect("explicit source folder");
    let capture = std::env::var_os("LOOMTERM_TEST_CAPTURE").expect("owned capture path");
    let mut size = PtySize {
        rows: 36,
        cols: 140,
        pixel_width: 0,
        pixel_height: 0,
    };
    let pair = native_pty_system().openpty(size).expect("real outer PTY");
    let mut command = CommandBuilder::new(env!("CARGO_BIN_EXE_loomterm"));
    command.cwd(cwd);
    command.env("LOOMTERM_ROOT", root);
    let mut reader = pair.master.try_clone_reader().unwrap();
    let mut writer = pair.master.take_writer().unwrap();
    // Both hosts can prepare their real outer terminals before one controller
    // barrier releases the two default CLI launches into an empty namespace.
    emit(json!({"event":"armed"}));
    let mut start = String::new();
    io::stdin().read_line(&mut start).unwrap();
    assert_eq!(
        serde_json::from_str::<Value>(&start).unwrap()["start"],
        true
    );
    let slave = pair.slave;
    let mut child: Option<OwnedChild> = None;
    let replies = Arc::new(Mutex::new((Vec::new(), false)));
    let sink = replies.clone();
    let mut terminal = Terminal::new(size.cols, size.rows, 1_048_576).unwrap();
    terminal
        .set_write_pty_callback(move |bytes| {
            let mut queue = sink.lock().unwrap();
            if queue.0.len() + bytes.len() <= 65_536 {
                queue.0.extend_from_slice(bytes);
            } else {
                queue.1 = true;
            }
        })
        .unwrap();
    let (tx, rx) = mpsc::sync_channel(32);
    let spawn_tx = tx.clone();
    let output_tx = tx.clone();
    std::thread::spawn(move || {
        let mut buffer = [0; 8192];
        loop {
            match reader.read(&mut buffer) {
                Ok(0) => {
                    let _ = output_tx.send(Event::Closed);
                    return;
                }
                Ok(n) => {
                    if output_tx.send(Event::Output(buffer[..n].to_vec())).is_err() {
                        return;
                    }
                }
                Err(error) => {
                    let _ = output_tx.send(Event::Error(error.to_string()));
                    return;
                }
            }
        }
    });
    std::thread::spawn(move || {
        let mut stdin = io::stdin().lock();
        loop {
            let mut line = Vec::new();
            let event = match (&mut stdin).take(65_537).read_until(b'\n', &mut line) {
                Ok(0) => break,
                Ok(_) if line.len() <= 65_536 => match serde_json::from_slice(&line) {
                    Ok(value) => Event::Command(value),
                    Err(error) => Event::Error(error.to_string()),
                },
                Ok(_) => Event::Error("controller input budget".into()),
                Err(error) => Event::Error(error.to_string()),
            };
            if tx.send(event).is_err() {
                return;
            }
        }
        let _ = tx.send(Event::Command(json!({"abort":true})));
    });
    let mut raw = File::create(capture).unwrap();
    // INHERIT_CURSOR can issue CPR while spawn_command is still in progress.
    // Keep servicing real terminal queries before waiting for spawn completion.
    std::thread::spawn(move || {
        let event = match slave.spawn_command(command) {
            Ok(child) => Event::Spawned(OwnedChild(child)),
            Err(error) => Event::Error(error.to_string()),
        };
        let _ = spawn_tx.send(event);
    });
    let mut total = 0usize;
    let deadline = Instant::now() + Duration::from_secs(180);
    let result = (|| -> Result<(), String> {
        loop {
            if Instant::now() >= deadline {
                return Err("outer host deadline".into());
            }
            match rx.recv_timeout(Duration::from_millis(20)) {
                Ok(Event::Spawned(spawned)) => {
                    emit(json!({"event":"ready","pid":spawned.0.process_id()}));
                    child = Some(spawned);
                }
                Ok(Event::Output(bytes)) => {
                    total += bytes.len();
                    if total > 8 * 1024 * 1024 {
                        return Err("outer output budget".into());
                    }
                    raw.write_all(&bytes).map_err(|e| e.to_string())?;
                    terminal.write(&bytes);
                    let bytes = {
                        let mut queue = replies.lock().unwrap();
                        if queue.1 {
                            return Err("outer terminal reply budget".into());
                        }
                        std::mem::take(&mut queue.0)
                    };
                    if !bytes.is_empty() {
                        writer.write_all(&bytes).map_err(|e| e.to_string())?;
                        writer.flush().map_err(|e| e.to_string())?;
                    }
                }
                Ok(Event::Command(value)) => {
                    if value["abort"] == true {
                        return Err("controller abort".into());
                    }
                    if let Some(input) = value["input"].as_str() {
                        writer
                            .write_all(input.as_bytes())
                            .map_err(|e| e.to_string())?;
                        writer.flush().map_err(|e| e.to_string())?;
                    }
                    if let Some(dims) = value["resize"].as_array() {
                        if dims.len() != 2 {
                            return Err("resize requires columns and rows".into());
                        }
                        size.cols = dims[0]
                            .as_u64()
                            .ok_or("columns")?
                            .try_into()
                            .map_err(|_| "columns")?;
                        size.rows = dims[1]
                            .as_u64()
                            .ok_or("rows")?
                            .try_into()
                            .map_err(|_| "rows")?;
                        if size.cols == 0 || size.rows == 0 || size.cols > 400 || size.rows > 160 {
                            return Err("outer grid budget".into());
                        }
                        pair.master.resize(size).map_err(|e| e.to_string())?;
                        terminal
                            .resize(size.cols, size.rows, 8, 16)
                            .map_err(|e| e.to_string())?;
                    }
                    let screen = terminal
                        .read_text_viewport((0, 0), (size.cols - 1, u32::from(size.rows - 1)), true)
                        .map_err(|e| e.to_string())?;
                    emit(
                        json!({"id":value["id"],"screen":screen,"bytes":total,"cols":size.cols,"rows":size.rows}),
                    );
                }
                Ok(Event::Closed) | Err(mpsc::RecvTimeoutError::Timeout) => {}
                Ok(Event::Error(error)) => return Err(error),
                Err(mpsc::RecvTimeoutError::Disconnected) => {
                    return Err("host event channel closed".into())
                }
            }
            if let Some(child) = child.as_mut() {
                if let Some(status) = child.0.try_wait().map_err(|e| e.to_string())? {
                    raw.flush().map_err(|e| e.to_string())?;
                    emit(json!({"event":"exit","code":status.exit_code(),"bytes":total}));
                    return Ok(());
                }
            }
        }
    })();
    if let Err(error) = result {
        // This handle names only the CLI child created by this host.
        if let Some(child) = child.as_mut() {
            child.terminate();
        }
        emit(json!({"event":"error","message":error}));
        panic!("{error}");
    }
}
