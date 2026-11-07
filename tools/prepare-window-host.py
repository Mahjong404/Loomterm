"""Apply the Window adapter to separately acquired fixed WezTerm source."""
import argparse
import hashlib
from pathlib import Path

REVISION = "e2a98e1b8af156988ad0ac778658ac27b60a55fb"
FILES = {
    "config/src/config.rs": "f92aade562c8ea22ae5ecccc6378f69cef80305f",
    "wezterm-gui/src/main.rs": "5e7ca47b34740368a5d3847761b645e635e7157c",
    "window/src/os/windows/window.rs": "715b67a965ef6e0d202450d44dc4b13edc6c891e",
    "wezterm-input-types/src/lib.rs": "f5673f600d8a06b4239a9a3c1146fc387123d8a8",
}


PATCHED = {
    "config/src/config.rs": "5596c82961066958e0338a74f796453cb1cf6ad0a80d6b538168aa4243697eb9",
    "wezterm-gui/src/main.rs": "c45b4fed76383f726dd69807f354f13cce0a169916813330ac5358bd1b82833b",
    "window/src/os/windows/window.rs": "36445ae37855ba19f8d9c7b15b0b70b2e8e7c5c55d23dc57a160a6855064ea2d",
    "wezterm-input-types/src/lib.rs": "1232e79e958c29e68f9cb43c9759ca2e3c27eec1cb59de5ee1d3a35012277fe5",
}


def verify(root):
    for name, expected in PATCHED.items():
        if hashlib.sha256((root / name).read_bytes()).hexdigest() != expected:
            raise ValueError(f"{name}: not the prepared adapter v2")


def adapted_windows_input(source):
    source = source.replace("use std::convert::TryInto;", "use std::convert::{TryFrom, TryInto};")
    source = source.replace("    dead_pending: Option<(Modifiers, u32)>,",
                            "    dead_pending: Option<(Modifiers, u32)>,\n    pending_utf16: Option<u16>,")
    source = source.replace("            dead_pending: None,",
                            "            dead_pending: None,\n            pending_utf16: None,")
    old = '''        (
            Some(KeyCode::Char(std::char::from_u32_unchecked(wparam as u32))),
            None,
        )'''
    assert source.count(old) == 1
    source = source.replace(old, '''        let Some(character) = decode_windows_char(&mut inner.pending_utf16, wparam) else {
            return Some(0);
        };
        (Some(KeyCode::Char(character)), None)''')
    # ToUnicode may also return one UTF-16 surrogate on a VK_PACKET key-up.
    source = source.replace("1 => Some(std::char::from_u32_unchecked(out[0] as u32)),",
                            "1 => std::char::from_u32(out[0] as u32),")
    source = source.replace("1 => Some(KeyCode::Char(std::char::from_u32_unchecked(out[0] as u32))),",
                            "1 => std::char::from_u32(out[0] as u32).map(KeyCode::Char),")
    # Layout dead-key probes have no streaming pair state; invalid units cannot
    # be char values. Keep those exceptional probes visibly lossy and safe.
    source = source.replace("std::char::from_u32_unchecked(out[0] as u32)",
                            "std::char::from_u32(out[0] as u32).unwrap_or('\\u{fffd}')")
    source = source.replace("    rc_from_hwnd(hwnd)?\n        .borrow_mut()\n        .events\n        .dispatch(WindowEvent::FocusChanged(false));",
                            "    let window = rc_from_hwnd(hwnd)?;\n    let mut inner = window.borrow_mut();\n    inner.pending_utf16 = None;\n    inner.events.dispatch(WindowEvent::FocusChanged(false));")
    helper = '''// Loomterm adapter: WM_CHAR carries UTF-16 code units, not Unicode scalars.
fn decode_windows_char(pending: &mut Option<u16>, value: usize) -> Option<char> {
    let Ok(unit) = u16::try_from(value) else {
        *pending = None;
        return None;
    };
    if (0xd800..=0xdbff).contains(&unit) {
        *pending = Some(unit);
        return None;
    }
    let high = pending.take();
    if (0xdc00..=0xdfff).contains(&unit) {
        return high.and_then(|high| char::from_u32(
            0x10000 + ((u32::from(high) - 0xd800) << 10) + u32::from(unit) - 0xdc00));
    }
    char::from_u32(u32::from(unit))
}

#[cfg(test)]
mod loomterm_utf16_tests {
    use super::decode_windows_char;
    #[test]
    fn character_messages_pair_surrogates_and_recover_from_invalid_units() {
        let mut pending = None;
        assert_eq!(decode_windows_char(&mut pending, 0x4e2d), Some('中'));
        assert_eq!(decode_windows_char(&mut pending, 0xd83d), None);
        assert_eq!(decode_windows_char(&mut pending, 0xde00), Some('😀'));
        assert_eq!(decode_windows_char(&mut pending, 0xde00), None);
        assert_eq!(decode_windows_char(&mut pending, 0xd83d), None);
        assert_eq!(decode_windows_char(&mut pending, 65), Some('A'));
        assert_eq!(decode_windows_char(&mut pending, usize::MAX), None);
        assert_eq!(pending, None);
    }
}

'''
    anchor = "unsafe fn key(hwnd: HWND, msg: UINT, wparam: WPARAM, lparam: LPARAM) -> Option<LRESULT> {"
    assert source.count(anchor) == 1
    return source.replace(anchor, helper + anchor)


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
    elif path == "window/src/os/windows/window.rs":
        return adapted_windows_input(source)
    elif path == "wezterm-input-types/src/lib.rs":
        old = '''                let uni = self.win32_uni_char.unwrap_or(*c) as u32;
                Some(format!(
                    "\\u{1b}[{};{};{};{};{};{}_",
                    vkey, scan_code, uni, key_down, control_key_state, self.repeat_count
                ))'''
        new = '''                // WM_CHAR has no physical VK: upstream may carry its UTF-16
                // unit in raw_code. Use VK=0 so clients decode it as text.
                let vkey = if vkey > 0xff && scan_code == 0 { 0 } else { vkey };
                // KEY_EVENT_RECORD carries UTF-16, including surrogate pairs.
                let mut units = [0u16; 2];
                let mut encoded = String::new();
                for uni in self.win32_uni_char.unwrap_or(*c).encode_utf16(&mut units) {
                    encoded.push_str(&format!(
                        "\\u{1b}[{};{};{};{};{};{}_",
                        vkey, scan_code, uni, key_down, control_key_state, self.repeat_count
                    ));
                }
                Some(encoded)'''
        assert source.count(old) == 1
        return source.replace(old, new) + '''
#[cfg(all(test, windows))]
mod loomterm_win32_utf16_tests {
    use super::*;
    #[test]
    fn non_bmp_scalar_uses_two_utf16_records_and_bmp_is_unchanged() {
        let mut key = KeyEvent {
            key: KeyCode::Char('中'), modifiers: Modifiers::NONE,
            leds: KeyboardLedStatus::empty(), repeat_count: 1,
            key_is_down: true, win32_uni_char: None,
            raw: Some(RawKeyEvent {
                key: KeyCode::RawCode(231), phys_code: None, raw_code: 231,
                scan_code: 0, modifiers: Modifiers::NONE,
                leds: KeyboardLedStatus::empty(), repeat_count: 1,
                key_is_down: true, handled: Handled::new(),
            }),
        };
        assert_eq!(key.encode_win32_input_mode().unwrap(), "\\x1b[231;0;20013;1;0;1_");
        key.key = KeyCode::Char('😀');
        assert_eq!(key.encode_win32_input_mode().unwrap(),
                   "\\x1b[231;0;55357;1;0;1_\\x1b[231;0;56832;1;0;1_");
        key.raw.as_mut().unwrap().raw_code = 56832;
        let esc = 27 as char;
        assert_eq!(key.encode_win32_input_mode().unwrap(),
                   format!("{esc}[0;0;55357;1;0;1_{esc}[0;0;56832;1;0;1_"));
    }
}
'''
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
    (root / ".tag").write_text("loomterm-window-e2a98e1b-adapter2\n", encoding="utf-8")


def upgrade(root):
    new_files = {"window/src/os/windows/window.rs", "wezterm-input-types/src/lib.rs"}
    for path, expected in PATCHED.items():
        if path not in new_files and hashlib.sha256((root / path).read_bytes()).hexdigest() != expected:
            raise ValueError("upgrade requires the exact adapter v1 source")
    changes = []
    for name in new_files:
        data = (root / name).read_bytes()
        if hashlib.sha1(f"blob {len(data)}\0".encode() + data).hexdigest() != FILES[name]:
            raise ValueError("upgrade requires the pristine fixed Windows input files")
        changes.append((root / name, adapted(name, data.decode("utf-8"))))
    for file, result in changes:
        file.write_bytes(result.encode("utf-8"))
    (root / ".tag").write_text("loomterm-window-e2a98e1b-adapter2\n", encoding="utf-8")


if __name__ == "__main__":
    parser = argparse.ArgumentParser(description=__doc__)
    parser.add_argument("source", type=Path)
    parser.add_argument("--verify", action="store_true")
    parser.add_argument("--upgrade-input", action="store_true", help="Upgrade only a verified adapter v1 copy")
    args = parser.parse_args()
    if args.verify and args.upgrade_input:
        parser.error("choose verification or upgrade")
    (verify if args.verify else upgrade if args.upgrade_input else prepare)(args.source.resolve())
