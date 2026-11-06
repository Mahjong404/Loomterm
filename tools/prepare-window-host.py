"""Apply the Window adapter to separately acquired fixed WezTerm source."""
import argparse
import hashlib
from pathlib import Path

REVISION = "e2a98e1b8af156988ad0ac778658ac27b60a55fb"
FILES = {
    "config/src/config.rs": "f92aade562c8ea22ae5ecccc6378f69cef80305f",
    "wezterm-gui/src/main.rs": "5e7ca47b34740368a5d3847761b645e635e7157c",
}


PATCHED = {
    "config/src/config.rs": "5596c82961066958e0338a74f796453cb1cf6ad0a80d6b538168aa4243697eb9",
    "wezterm-gui/src/main.rs": "c45b4fed76383f726dd69807f354f13cce0a169916813330ac5358bd1b82833b",
}


def verify(root):
    for name, expected in PATCHED.items():
        if hashlib.sha256((root / name).read_bytes()).hexdigest() != expected:
            raise ValueError(f"{name}: not the prepared adapter v1")


def adapted(path, source):
    if path == "config/src/config.rs":
        for kind in ("cache", "data", "runtime"):
            start = source.index(f"pub(crate) fn compute_{kind}_dir()")
            end = source.index("\n}\n", start) + 2
            source = source[:start] + f'''pub(crate) fn compute_{kind}_dir() -> anyhow::Result<PathBuf> {{
    // Loomterm Window adapter v1. Preserve ordinary Shell HOME/tool storage.
    let root = std::env::var_os("LOOMTERM_WINDOW_ROOT")
        .ok_or_else(|| anyhow::anyhow!("LOOMTERM_WINDOW_ROOT is required"))?;
    let root = PathBuf::from(root);
    anyhow::ensure!(root.is_absolute(), "Window root must be absolute");
    Ok(root.join("{kind}"))
}}''' + source[end:]
    else:
        assert source.count('wide_string("org.wezfurlong.wezterm")') == 1
        source = source.replace('wide_string("org.wezfurlong.wezterm")', 'wide_string("org.loomterm.window")')
        assert source.count("fn main() {") == 1
        source = source.replace("fn main() {", '''fn main() {
    // Loomterm adapter v1. Probe before logging/config or native resources.
    if std::env::args_os().nth(1).as_deref() == Some(std::ffi::OsStr::new("--loomterm-window-contract")) {
        std::process::exit(42);
    }
    let root = std::env::var_os("LOOMTERM_WINDOW_ROOT")
        .expect("start through loomterm-window");
    assert!(std::path::Path::new(&root).is_absolute(), "Window root must be absolute");''')
    return source


def prepare(root):
    changes = []
    for path, expected in FILES.items():
        file = root / path
        data = file.read_bytes()
        actual = hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest()
        if actual != expected:
            raise ValueError(f"{path}: expected fixed Git blob {expected}, got {actual}")
        changes.append((file, adapted(path, data.decode("utf-8"))))
    for file, result in changes:
        file.write_bytes(result.encode("utf-8"))
    (root / ".tag").write_text("loomterm-window-e2a98e1b-adapter1\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("--verify", action="store_true")
    args = parser.parse_args()
    (verify if args.verify else prepare)(args.source.resolve())
