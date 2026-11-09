//! Loomterm product identity, paths and enabled integration policy.
//! User HOME/XDG/Agent storage remains the user's environment, not product state.
use std::io::{self, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::{
    atomic::{AtomicBool, Ordering},
    OnceLock,
};

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

const ROOT_MARKER: &str = "loomterm-product.json";
const MARKER: &[u8] = b"{\"product\":\"loomterm\",\"paths_version\":1}\n";
static PATHS: OnceLock<ProductPaths> = OnceLock::new();
static NO_PROFILE: AtomicBool = AtomicBool::new(false);
static IDENTITY: OnceLock<InstanceIdentity> = OnceLock::new();

// M1 allocation safety ceilings, not measured performance guarantees.
pub(crate) const MAX_TERMINAL_COLS: u16 = 1000;
pub(crate) const MAX_TERMINAL_ROWS: u16 = 500;
pub(crate) const MAX_TERMINAL_CELLS: usize = 500_000;

pub(crate) fn terminal_geometry_error(cols: u16, rows: u16) -> Option<&'static str> {
    (cols > MAX_TERMINAL_COLS
        || rows > MAX_TERMINAL_ROWS
        || usize::from(cols) * usize::from(rows) > MAX_TERMINAL_CELLS)
        .then_some(
            "terminal grid exceeds the M1 safety limit (1000 columns, 500 rows, 500000 cells)",
        )
}

#[derive(Debug, Clone, Default, Deserialize, Serialize)]
#[serde(default, deny_unknown_fields)]
pub struct ProductPolicy {
    /// Skip PowerShell profiles when explicitly requested; default preserves user behavior.
    pub shell_no_profile: bool,
    /// Reserved for the separately accepted Agent integration stage; M1 rejects true.
    pub agent_integrations: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct ProductPaths {
    pub root: PathBuf,
    pub config: PathBuf,
    pub state: PathBuf,
    pub data: PathBuf,
    pub cache: PathBuf,
    pub runtime: PathBuf,
    pub logs: PathBuf,
}

fn invalid(message: impl Into<String>) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidInput, message.into())
}

/// Normalize both existing and not-yet-created endpoints without appending an empty suffix.
pub fn canonical_path(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() || path.components().any(|p| matches!(p, Component::ParentDir)) {
        return Err(invalid(
            "Loomterm paths must be absolute and must not contain '..'",
        ));
    }
    let ancestor = path
        .ancestors()
        .find(|p| p.exists())
        .ok_or_else(|| invalid("no existing path ancestor"))?;
    let canonical = ancestor.canonicalize()?;
    let suffix = path.strip_prefix(ancestor).map_err(io::Error::other)?;
    Ok(if suffix.as_os_str().is_empty() {
        canonical
    } else {
        canonical.join(suffix)
    })
}

impl ProductPaths {
    pub fn at_root(root: &Path) -> io::Result<Self> {
        let root = canonical_path(root)?;
        Ok(Self {
            config: root.join("config"),
            state: root.join("state"),
            data: root.join("data"),
            cache: root.join("cache"),
            runtime: root.join("runtime"),
            logs: root.join("logs"),
            root,
        })
    }

    fn discover() -> io::Result<Self> {
        if let Some(root) = std::env::var_os("LOOMTERM_ROOT") {
            return Self::at_root(Path::new(&root));
        }
        #[cfg(windows)]
        {
            let base = std::env::var_os("LOCALAPPDATA").ok_or_else(|| {
                invalid("LOCALAPPDATA is unavailable; set LOOMTERM_ROOT explicitly")
            })?;
            Self::at_root(&PathBuf::from(base).join("Loomterm"))
        }
        #[cfg(target_os = "macos")]
        {
            let home = PathBuf::from(
                std::env::var_os("HOME").ok_or_else(|| invalid("HOME is unavailable"))?,
            );
            let mut paths = Self::at_root(&home.join("Library/Application Support/Loomterm"))?;
            paths.cache = canonical_path(&home.join("Library/Caches"))?.join("Loomterm");
            Ok(paths)
        }
        #[cfg(not(any(windows, target_os = "macos")))]
        {
            let home = PathBuf::from(
                std::env::var_os("HOME").ok_or_else(|| invalid("HOME is unavailable"))?,
            );
            let xdg = |key: &str, fallback: &str| -> io::Result<PathBuf> {
                Ok(canonical_path(
                    &std::env::var_os(key)
                        .map(PathBuf::from)
                        .unwrap_or_else(|| home.join(fallback)),
                )?
                .join("loomterm"))
            };
            let root = xdg("XDG_DATA_HOME", ".local/share")?;
            Ok(Self {
                root: root.clone(),
                config: xdg("XDG_CONFIG_HOME", ".config")?,
                state: xdg("XDG_STATE_HOME", ".local/state")?,
                data: root,
                cache: xdg("XDG_CACHE_HOME", ".cache")?,
                runtime: match std::env::var_os("XDG_RUNTIME_DIR") {
                    Some(p) => canonical_path(&PathBuf::from(p))?.join("loomterm"),
                    None => xdg("XDG_STATE_HOME", ".local/state")?.join("runtime"),
                },
                logs: xdg("XDG_STATE_HOME", ".local/state")?.join("logs"),
            })
        }
    }

    pub fn endpoint(&self, session: &str, client: bool) -> PathBuf {
        self.runtime.join(session).join(if client {
            "loomterm-client.sock"
        } else {
            "loomterm.sock"
        })
    }

    pub fn check_endpoint(&self, path: &Path, session: &str, client: bool) -> io::Result<PathBuf> {
        let expected = self.endpoint(session, client);
        if pipe_name(path)? != pipe_name(&expected)? {
            return Err(invalid(
                "endpoint is not this Loomterm product/session endpoint",
            ));
        }
        Ok(expected)
    }

    /// Explicit session management may discover a sibling namespace, but never
    /// an arbitrary socket or another product's runtime directory.
    pub fn endpoint_session(&self, path: &Path) -> io::Result<String> {
        let path = canonical_path(path)?;
        let relative = path
            .strip_prefix(canonical_path(&self.runtime)?)
            .map_err(|_| invalid("endpoint is outside the Loomterm runtime directory"))?;
        let parts: Vec<_> = relative.components().collect();
        if parts.len() != 2 {
            return Err(invalid("endpoint must belong to one Loomterm session"));
        }
        let session = parts[0]
            .as_os_str()
            .to_str()
            .ok_or_else(|| invalid("invalid session name"))?;
        crate::session::validate_name(session).map_err(invalid)?;
        if !matches!(
            parts[1].as_os_str().to_str(),
            Some("loomterm.sock" | "loomterm-client.sock")
        ) {
            return Err(invalid("unknown Loomterm endpoint role"));
        }
        Ok(session.to_owned())
    }

    fn create(&mut self) -> io::Result<()> {
        let marker = self.root.join(ROOT_MARKER);
        if self.root.exists()
            && !marker.exists()
            && std::fs::read_dir(&self.root)?.next().is_some()
            && !marker.exists()
        {
            return Err(invalid(
                "LOOMTERM_ROOT is nonempty and is not an initialized Loomterm product directory",
            ));
        }
        create_product_directory(&self.root)?;
        match std::fs::OpenOptions::new()
            .write(true)
            .create_new(true)
            .open(&marker)
        {
            Ok(mut f) => {
                f.write_all(MARKER)?;
                f.sync_all()?;
            }
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                let mut valid = false;
                for _ in 0..20 {
                    let bytes = std::fs::read(&marker)?;
                    if bytes == MARKER {
                        valid = true;
                        break;
                    }
                    if !bytes.is_empty() {
                        break;
                    }
                    std::thread::sleep(std::time::Duration::from_millis(10));
                }
                if !valid {
                    return Err(invalid("invalid Loomterm product identity marker"));
                }
            }
            Err(e) => return Err(e),
        }
        self.root = self.root.canonicalize()?;
        for dir in [
            &self.root,
            &self.config,
            &self.state,
            &self.data,
            &self.cache,
            &self.runtime,
            &self.logs,
        ] {
            create_product_directory(dir)?;
        }
        Ok(())
    }
}

fn create_product_directory(path: &Path) -> io::Result<()> {
    #[cfg(unix)]
    {
        crate::platform::unix_paths::create_private_directory(path)
    }
    #[cfg(not(unix))]
    {
        std::fs::create_dir_all(path)
    }
}

pub fn paths() -> ProductPaths {
    PATHS
        .get()
        .cloned()
        .unwrap_or_else(|| ProductPaths::discover().expect("Loomterm product paths"))
}
pub fn session_name() -> String {
    let name = crate::session::active_name().unwrap_or_else(|| "default".into());
    #[cfg(windows)]
    let name = name.to_ascii_lowercase();
    name
}

pub fn initialize() -> io::Result<()> {
    let mut paths = ProductPaths::discover()?;
    paths.create()?;
    PATHS
        .set(paths)
        .map_err(|_| invalid("product paths initialized twice"))?;
    let runtime = self::paths().runtime.join(session_name());
    create_product_directory(&runtime)?;
    std::env::set_var("LOOMTERM_GRAPHICS_DIR", runtime.join("graphics"));
    Ok(())
}

pub fn configure(policy: &ProductPolicy) -> io::Result<()> {
    if policy.agent_integrations {
        return Err(invalid("Agent integrations are not enabled in M1; ordinary Agent CLIs may run explicitly inside a Shell"));
    }
    NO_PROFILE.store(policy.shell_no_profile, Ordering::Release);
    Ok(())
}

pub fn validate_config(config: &crate::config::Config) -> io::Result<()> {
    if config.advanced.scrollback_limit_bytes > 16 * 1024 * 1024 {
        return Err(invalid("M1 scrollback limit is at most 16 MiB per Terminal; the instance admits at most 64 live Terminals"));
    }
    configure(&config.loomterm)
}

pub fn validate_overrides() -> io::Result<()> {
    let p = paths();
    let session = session_name();
    for (key, client) in [
        (crate::api::SOCKET_PATH_ENV_VAR, false),
        (
            crate::server::socket_paths::CLIENT_SOCKET_PATH_ENV_VAR,
            true,
        ),
    ] {
        if let Some(value) =
            std::env::var_os(key).filter(|_| !crate::session::explicit_session_requested())
        {
            p.check_endpoint(Path::new(&value), &session, client)?;
        }
    }
    if let Some(value) = std::env::var_os("LOOMTERM_CONFIG_PATH") {
        let config = canonical_path(Path::new(&value))?;
        if !config.starts_with(canonical_path(&p.config)?) {
            return Err(invalid(
                "config override is outside the Loomterm config directory",
            ));
        }
    }
    Ok(())
}

pub fn validate_endpoint_target(path: &Path) -> io::Result<()> {
    // Unit fixtures construct isolated transport objects without running main.
    // Every real entry initializes PATHS before opening any product connection.
    if let Some(p) = PATHS.get() {
        p.endpoint_session(path)?;
    }
    Ok(())
}
pub fn shell_no_profile() -> bool {
    NO_PROFILE.load(Ordering::Acquire)
}
pub fn agent_integrations_enabled() -> bool {
    false
}
pub fn remote_enabled() -> bool {
    false
}

/// This namespace is independent from original Herdr, even at the same wire version.
pub fn pipe_name(path: &Path) -> io::Result<String> {
    let canonical = canonical_path(path)?;
    let value = canonical.to_string_lossy();
    let value = value
        .strip_prefix("\\\\?\\")
        .unwrap_or(&value)
        .replace('\\', "/");
    #[cfg(windows)]
    let value = value.to_ascii_lowercase();
    Ok(format!(
        "loomterm-v1-{:x}",
        Sha256::digest(value.as_bytes())
    ))
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, schemars::JsonSchema)]
pub struct InstanceIdentity {
    pub product: String,
    pub namespace: String,
    pub instance_id: String,
    pub pid: u32,
}
pub fn instance_identity() -> InstanceIdentity {
    IDENTITY
        .get_or_init(|| InstanceIdentity {
            product: "loomterm".into(),
            namespace: session_name(),
            instance_id: format!(
                "{}-{}",
                std::process::id(),
                std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .unwrap_or_default()
                    .as_nanos()
            ),
            pid: std::process::id(),
        })
        .clone()
}
pub fn check_identity(identity: Option<&InstanceIdentity>) -> io::Result<()> {
    if identity.is_some_and(|i| {
        i.product == "loomterm"
            && i.namespace == session_name()
            && i.pid != 0
            && !i.instance_id.is_empty()
    }) {
        Ok(())
    } else {
        Err(invalid(
            "server identity is not the selected Loomterm product/session",
        ))
    }
}

pub fn method_enabled(method: &crate::api::schema::Method) -> bool {
    let name = method.operation().name;
    ![
        "agent.",
        "pane.report_agent",
        "plugin.",
        "integration.",
        "worktree.",
        "notification.",
        "server.ssh_agent.",
        "server.live_handoff",
        "server.reload_agent_manifests",
    ]
    .iter()
    .any(|prefix| name.starts_with(prefix))
}

pub fn describe() -> io::Result<serde_json::Value> {
    let p = paths();
    let session = session_name();
    let api = p.endpoint(&session, false);
    let client = p.endpoint(&session, true);
    Ok(
        serde_json::json!({"product":"loomterm","paths":p,"session":session,"api_endpoint":api,"client_endpoint":client,
        "api_pipe":format!("\\\\.\\pipe\\{}",pipe_name(&api)?),"client_pipe":format!("\\\\.\\pipe\\{}",pipe_name(&client)?),"daemon_started":false}),
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn m1_terminal_grid_ceiling_rejects_each_axis_before_allocation() {
        assert!(terminal_geometry_error(MAX_TERMINAL_COLS, MAX_TERMINAL_ROWS).is_none());
        assert_eq!(
            usize::from(MAX_TERMINAL_COLS) * usize::from(MAX_TERMINAL_ROWS),
            MAX_TERMINAL_CELLS
        );
        for (cols, rows) in [(1001, 1), (1, 501), (u16::MAX, u16::MAX)] {
            assert!(terminal_geometry_error(cols, rows).is_some());
        }
    }
    fn temp() -> PathBuf {
        std::env::temp_dir().join(format!(
            "loomterm-path-test-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ))
    }
    #[test]
    fn endpoint_identity_is_stable_when_marker_appears_and_paths_have_unicode_spaces() {
        let base = temp();
        std::fs::create_dir_all(&base).unwrap();
        let mut paths = ProductPaths::at_root(&base.join("中文 项目")).unwrap();
        paths.create().unwrap();
        let api = paths.endpoint("default", false);
        std::fs::create_dir_all(api.parent().unwrap()).unwrap();
        let before = pipe_name(&api).unwrap();
        std::fs::write(&api, b"marker").unwrap();
        assert_eq!(before, pipe_name(&api).unwrap());
        assert_eq!(
            canonical_path(&api).unwrap().file_name(),
            Some(std::ffi::OsStr::new("loomterm.sock"))
        );
        assert!(paths
            .check_endpoint(&paths.endpoint("wrong", false), "default", false)
            .is_err());
        assert!(paths
            .check_endpoint(&paths.endpoint("default", true), "default", false)
            .is_err());
        std::fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn unknown_nonempty_root_is_rejected_without_writing_marker() {
        let base = temp();
        std::fs::create_dir_all(&base).unwrap();
        std::fs::write(base.join("foreign-canary"), b"untouched").unwrap();
        let mut paths = ProductPaths::at_root(&base).unwrap();
        assert!(paths.create().is_err());
        assert!(!base.join(ROOT_MARKER).exists());
        assert_eq!(
            std::fs::read(base.join("foreign-canary")).unwrap(),
            b"untouched"
        );
        std::fs::remove_dir_all(base).unwrap();
    }
    #[test]
    fn relative_or_parent_traversal_root_is_rejected() {
        assert!(ProductPaths::at_root(Path::new("relative")).is_err());
        assert!(ProductPaths::at_root(&std::env::temp_dir().join("../foreign")).is_err());
    }
    #[test]
    fn wrong_product_or_namespace_is_rejected() {
        let mut identity = instance_identity();
        identity.product = "herdr".into();
        assert!(check_identity(Some(&identity)).is_err());
        identity.product = "loomterm".into();
        identity.namespace = "another".into();
        assert!(check_identity(Some(&identity)).is_err());
        assert!(check_identity(None).is_err());
    }

    #[test]
    fn sibling_sessions_are_discoverable_but_foreign_endpoints_are_rejected() {
        let base = temp();
        std::fs::create_dir_all(&base).unwrap();
        let paths = ProductPaths::at_root(&base).unwrap();
        assert_eq!(
            paths
                .endpoint_session(&paths.endpoint("another", false))
                .unwrap(),
            "another"
        );
        assert!(paths
            .endpoint_session(&base.join("foreign/another/loomterm.sock"))
            .is_err());
        assert!(paths
            .endpoint_session(&paths.runtime.join("another/herdr.sock"))
            .is_err());
        assert!(paths
            .endpoint_session(&paths.runtime.join("another/deeper/loomterm.sock"))
            .is_err());
        std::fs::remove_dir_all(base).unwrap();
    }
}
