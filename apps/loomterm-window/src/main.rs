#![cfg_attr(all(windows, not(test)), windows_subsystem = "windows")]

use sha2::{Digest, Sha256};
use std::io::Read;
use std::{env, ffi::OsString, fs, io, path::PathBuf, process::Command};

fn verify_host(path: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    // Verify the build-produced manifest before executing even a contract probe.
    // A plain upstream binary may initialize user logs before parsing its argv.
    let expected = fs::read_to_string(path.with_file_name("loomterm-host.sha256"))?;
    let mut file = fs::File::open(path)?;
    let mut hash = Sha256::new();
    let mut buffer = [0; 65536];
    loop {
        let count = file.read(&mut buffer)?;
        if count == 0 {
            break;
        }
        hash.update(&buffer[..count]);
    }
    if format!("{:x}", hash.finalize()) != expected.trim() {
        return Err("host does not match the prepared build manifest".into());
    }
    Ok(())
}

fn verify_image_runtime(cli: &std::path::Path) -> Result<(), Box<dyn std::error::Error>> {
    // Without the adjacent bundle the CLI loader uses system ConPTY, which
    // silently drops Kitty APC on the Windows build verified for this Window.
    // The CLI loader remains responsible for exact hashes and native loading.
    let dir = cli.parent().ok_or("CLI has no parent directory")?;
    for relative in [
        "conpty/conpty.dll",
        "conpty/x64/OpenConsole.exe",
        "conpty/arm64/OpenConsole.exe",
        "conpty/herdr-conpty.json",
    ] {
        if !dir.join(relative).is_file() {
            return Err(format!("Loomterm Window requires staged {relative}").into());
        }
    }
    Ok(())
}

#[derive(Debug)]
struct Options {
    host: PathBuf,
    cli: PathBuf,
    root: PathBuf,
    cwd: PathBuf,
    session: OsString,
    check: bool,
}

fn options(args: impl IntoIterator<Item = OsString>) -> Result<Options, String> {
    let bin = env::current_exe().map_err(|e| e.to_string())?;
    let dir = bin.parent().ok_or("no executable directory")?;
    let mut o = Options {
        host: dir.join("host/wezterm-gui.exe"),
        cli: dir.join("loomterm.exe"),
        root: env::var_os("LOOMTERM_ROOT")
            .map(PathBuf::from)
            .unwrap_or_else(|| {
                PathBuf::from(env::var_os("LOCALAPPDATA").unwrap_or_default()).join("Loomterm")
            }),
        cwd: env::current_dir().map_err(|e| e.to_string())?,
        session: OsString::from("default"),
        check: false,
    };
    let mut args = args.into_iter();
    while let Some(arg) = args.next() {
        if arg == "--check" {
            o.check = true;
            continue;
        }
        let value = args.next().ok_or("option requires a value")?;
        match arg.to_str() {
            Some("--host") => o.host = value.into(),
            Some("--cli") => o.cli = value.into(),
            Some("--root") => o.root = value.into(),
            Some("--cwd") => o.cwd = value.into(),
            Some("--session") => o.session = value,
            _ => return Err(format!("unknown option: {}", arg.to_string_lossy())),
        }
    }
    if !o.root.is_absolute() || !o.cwd.is_absolute() {
        return Err("root and cwd must be absolute".into());
    }
    Ok(o)
}

fn command(o: &Options, config: &std::path::Path) -> Command {
    let mut c = Command::new(&o.host);
    // Host discovery/control environment must not attach another installation.
    for (key, _) in env::vars_os() {
        if key
            .to_string_lossy()
            .to_ascii_uppercase()
            .starts_with("WEZTERM_")
        {
            c.env_remove(key);
        }
    }
    c.env("LOOMTERM_ROOT", &o.root)
        .env("LOOMTERM_WINDOW_ROOT", o.root.join("window"))
        // Never allow a caller's legacy override to disable the verified
        // app-local image path for this Window's TUI and cold-started Daemon.
        .env_remove("HERDR_WINDOWS_CONPTY")
        .current_dir(&o.cwd)
        .args([OsString::from("--config-file"), config.as_os_str().into()])
        .args([
            "start",
            "--no-auto-connect",
            "--always-new-process",
            "--class",
            "LoomtermWindow",
            "--cwd",
        ])
        .arg(&o.cwd)
        .arg("--")
        .arg(&o.cli)
        .arg("--session")
        .arg(&o.session);
    c
}

fn run(o: Options) -> Result<(), Box<dyn std::error::Error>> {
    if !cfg!(windows) {
        return Err("Loomterm Window currently requires Windows".into());
    }
    let cli = fs::canonicalize(&o.cli)?;
    let host = fs::canonicalize(&o.host)?;
    verify_host(&host)?;
    verify_image_runtime(&cli)?;
    let o = Options { cli, host, ..o };
    // Reuse the production root/session validator without starting a Daemon.
    let mut validator = Command::new(&o.cli);
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        validator.creation_flags(0x08000000); // CREATE_NO_WINDOW for --check only.
    }
    let validation = validator
        .env("LOOMTERM_ROOT", &o.root)
        .args([
            OsString::from("--session"),
            o.session.clone(),
            OsString::from("--check"),
        ])
        .output()?;
    if !validation.status.success() {
        return Err(format!(
            "CLI validation failed: {}{}",
            String::from_utf8_lossy(&validation.stdout),
            String::from_utf8_lossy(&validation.stderr)
        )
        .into());
    }
    let window = o.root.join("window");
    fs::create_dir_all(&window)?;
    let config = window.join("host-v1.lua");
    // Per-launch file avoids readers observing a concurrent partial write.
    let config = config.with_file_name(format!("host-v1-{}.lua", std::process::id()));
    fs::write(&config, include_str!("../host.lua"))?;
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        let contract = Command::new(&o.host)
            .arg("--loomterm-window-contract")
            .status()?;
        if contract.code() != Some(42) {
            return Err(
                "host lacks Loomterm path/identity adapter v1; refusing upstream host".into(),
            );
        }
        let mut c = command(&o, &config);
        if o.check {
            println!("{c:?}");
        } else {
            let status = c.status()?;
            if !status.success() {
                return Err(format!("Window host exited: {status}").into());
            }
        }
        Ok(())
    })();
    fs::remove_file(config)?;
    result
}

fn main() {
    let result = options(env::args_os().skip(1))
        .map_err(io::Error::other)
        .map_err(|e| Box::new(e) as Box<dyn std::error::Error>)
        .and_then(run);
    if let Err(error) = result {
        eprintln!("Loomterm Window: {error}");
        // GUI launches have no console; preserve a diagnosable startup failure.
        let log = env::temp_dir().join(format!("loomterm-window-error-{}.log", std::process::id()));
        let _ = fs::write(log, error.to_string());
        std::process::exit(1);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn argv_keeps_unicode_and_quoted_config_path() {
        let root = env::temp_dir().join("Loomterm test 中文");
        let o = options([
            OsString::from("--root"),
            root.into(),
            OsString::from("--session"),
            OsString::from("test"),
        ])
        .unwrap();
        let c = command(&o, std::path::Path::new("C:/config O'Neil.lua"));
        let args: Vec<_> = c.get_args().collect();
        assert_eq!(*args.last().unwrap(), "test");
        assert_eq!(args[1], "C:/config O'Neil.lua");
        assert!(args.contains(&std::ffi::OsStr::new("--always-new-process")));
        assert_eq!(args[args.len() - 3], o.cli.as_os_str());
        assert!(c
            .get_envs()
            .any(|(key, value)| key == "HERDR_WINDOWS_CONPTY" && value.is_none()));
    }

    #[test]
    fn window_rejects_missing_image_runtime_before_host_launch() {
        let dir = env::temp_dir().join(format!(
            "loomterm-image-runtime-test-{}",
            std::process::id()
        ));
        fs::create_dir(&dir).unwrap();
        let cli = dir.join("loomterm.exe");
        fs::write(&cli, b"controlled test bytes").unwrap();
        assert!(verify_image_runtime(&cli).is_err());
        for relative in [
            "conpty/conpty.dll",
            "conpty/x64/OpenConsole.exe",
            "conpty/arm64/OpenConsole.exe",
            "conpty/herdr-conpty.json",
        ] {
            let path = dir.join(relative);
            fs::create_dir_all(path.parent().unwrap()).unwrap();
            fs::write(path, b"fixture").unwrap();
        }
        verify_image_runtime(&cli).unwrap();
        fs::remove_dir_all(dir).unwrap();
    }

    #[test]
    fn host_manifest_detects_tampering_before_execution() {
        let dir = env::temp_dir().join(format!("loomterm-host-test-{}", std::process::id()));
        fs::create_dir(&dir).unwrap();
        let host = dir.join("host.exe");
        let manifest = dir.join("loomterm-host.sha256");
        fs::write(&host, b"controlled test bytes").unwrap();
        assert!(verify_host(&host).is_err());
        fs::write(
            &manifest,
            format!("{:x}", Sha256::digest(b"controlled test bytes")),
        )
        .unwrap();
        verify_host(&host).unwrap();
        fs::write(&host, b"changed").unwrap();
        assert!(verify_host(&host).is_err());
        fs::remove_file(host).unwrap();
        fs::remove_file(manifest).unwrap();
        fs::remove_dir(dir).unwrap();
    }

    #[test]
    fn rejects_relative_root_and_unknown_options() {
        assert!(options(["--root".into(), "relative".into()]).is_err());
        assert!(options(["--unknown".into(), "value".into()]).is_err());
        assert!(options(["--session".into()]).is_err());
    }
}
