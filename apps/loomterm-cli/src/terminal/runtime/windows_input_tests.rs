//! Opt-in conversion control through the production runtime/PTY actor/engine.
//! No client, private configuration, Shell or Agent is involved.
use super::*;
use std::time::Duration;

struct OwnedRuntime(Option<TerminalRuntime>);

impl Drop for OwnedRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown();
        }
    }
}

#[tokio::test]
#[ignore = "requires an explicit synthetic fixture, isolated Root and outer watchdog"]
async fn m2_windows_input_api_control() {
    assert!(
        std::env::var_os("LOOMTERM_ROOT").is_some(),
        "explicit test Root"
    );
    crate::product::initialize().expect("isolated product paths");
    let root = crate::product::paths();
    let fixture = std::path::PathBuf::from(
        std::env::var_os("LOOMTERM_TEST_INPUT_FIXTURE").expect("controlled fixture path"),
    );
    let mode = std::env::var("LOOMTERM_TEST_INPUT_API").expect("explicit API");
    assert!(matches!(mode.as_str(), "bytes" | "wide" | "records"));
    let (events, mut rx) = mpsc::channel(128);
    let runtime = TerminalRuntime::spawn_argv_command(
        PaneId::from_raw(1),
        100,
        120,
        root.root.clone(),
        &[fixture.to_str().unwrap().into(), mode.clone()],
        &crate::pane::PaneLaunchEnv::default(),
        crate::pane::AgentDetection::Disabled,
        1_048_576,
        crate::terminal_theme::TerminalTheme::default(),
        None,
        events,
        Arc::new(Notify::new()),
        Arc::new(RenderSignal::new()),
    )
    .unwrap();
    let owned = OwnedRuntime(Some(runtime));
    let runtime = owned.0.as_ref().unwrap();
    let child_pid = runtime.child_pid().expect("real ConPTY child");
    use windows_sys::Win32::Foundation::{CloseHandle, FILETIME};
    use windows_sys::Win32::System::Threading::{
        GetProcessTimes, OpenProcess, WaitForSingleObject, PROCESS_QUERY_LIMITED_INFORMATION,
        SYNCHRONIZATION_SYNCHRONIZE,
    };
    // SAFETY: the runtime just created this PID. Retain its process handle,
    // record creation time and wait on that exact identity, never a process name.
    let child = unsafe {
        OpenProcess(
            PROCESS_QUERY_LIMITED_INFORMATION | SYNCHRONIZATION_SYNCHRONIZE,
            0,
            child_pid,
        )
    };
    assert!(!child.is_null());
    let mut times = [FILETIME::default(); 4];
    let [created, exited, kernel, user] = &mut times;
    assert_ne!(
        unsafe { GetProcessTimes(child, created, exited, kernel, user) },
        0
    );
    let creation_time =
        (u64::from(times[0].dwHighDateTime) << 32) | u64::from(times[0].dwLowDateTime);
    use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW};
    let module_name: Vec<u16> = "conpty.dll\0".encode_utf16().collect();
    let mut module_path = [0u16; 32768];
    let module = unsafe { GetModuleHandleW(module_name.as_ptr()) };
    let conpty = if module.is_null() {
        "system kernel32".to_owned()
    } else {
        let len = unsafe {
            GetModuleFileNameW(module, module_path.as_mut_ptr(), module_path.len() as u32)
        };
        String::from_utf16(&module_path[..len as usize]).unwrap()
    };
    let ready = format!("CLI_INPUT_READY_{mode}_CP65001_MODE00000280");
    tokio::time::timeout(Duration::from_secs(8), async {
        while !runtime.visible_text().contains(&ready) {
            while rx.try_recv().is_ok() {}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture CP/mode readiness");
    let payload = "a中e\u{301}😀🚀";
    runtime
        .try_send_bytes(Bytes::from(format!("{payload}\x04")))
        .unwrap();
    tokio::time::timeout(Duration::from_secs(8), async {
        while !runtime.visible_text().contains("CLI_INPUT_DONE_0") {
            while rx.try_recv().is_ok() {}
            tokio::time::sleep(Duration::from_millis(10)).await;
        }
    })
    .await
    .expect("fixture bounded read/EOF");
    let text = runtime.visible_text();
    let prefix = match mode.as_str() {
        "bytes" => "CLI_B_",
        "wide" => "CLI_W_",
        _ => "CLI_R_",
    };
    let actual: String = text
        .lines()
        .filter_map(|line| line.trim().strip_prefix(prefix))
        .collect();
    let expected = if mode == "bytes" {
        payload
            .as_bytes()
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect::<String>()
    } else {
        payload
            .encode_utf16()
            .map(|unit| format!("{unit:04x}"))
            .collect::<String>()
    };
    let naturally_exited = unsafe { WaitForSingleObject(child, 5000) } == 0;
    unsafe { CloseHandle(child) };
    println!(
        "{}",
        serde_json::json!({"api":mode,"child_pid":child_pid,"child_creation_time":creation_time,"fixture":fixture,"conpty":conpty,"naturally_exited":naturally_exited,"input_utf8_hex":"61e4b8ad65cc81f09f9880f09f9a80","expected":expected,"actual":actual,"cp":65001,"mode":"0x280","record_events":text.lines().filter(|line| line.starts_with("CLI_RECORD_")).collect::<Vec<_>>()})
    );
    drop(owned);
    assert!(
        naturally_exited,
        "fixture must exit normally before resource cleanup"
    );
    assert_eq!(
        actual, expected,
        "input conversion boundary; no Window/TUI involved"
    );
}
