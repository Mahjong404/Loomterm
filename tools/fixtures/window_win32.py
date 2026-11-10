"""Scoped native Windows Window-test boundary; never discovers targets by title.

The caller supplies the exact owned host PID. SetWindowPos triggers real window
messages; SC_CLOSE follows the non-client titlebar/system-menu close path.
PrintWindow captures only that window and may fail with GPU renderers.
"""
import ctypes as c
from ctypes import wintypes as w
import os
import time

if os.name != "nt":
    raise RuntimeError("Windows native fixture only")

u = c.WinDLL("user32", use_last_error=True)
g = c.WinDLL("gdi32", use_last_error=True)
# Native geometry and PrintWindow pixels must share a physical DPI coordinate space.
u.SetThreadDpiAwarenessContext.argtypes = [c.c_void_p]
u.SetThreadDpiAwarenessContext.restype = c.c_void_p
assert u.SetThreadDpiAwarenessContext(c.c_void_p(-4)), "per-monitor-v2 context unavailable"

u.GetWindowTextW.argtypes = [w.HWND, w.LPWSTR, c.c_int]
u.GetWindowThreadProcessId.argtypes = [w.HWND, c.POINTER(w.DWORD)]
u.GetWindowThreadProcessId.restype = w.DWORD
u.GetClientRect.argtypes = [w.HWND, c.POINTER(w.RECT)]
u.GetWindowRect.argtypes = [w.HWND, c.POINTER(w.RECT)]
u.GetDpiForWindow.argtypes = [w.HWND]
u.GetDpiForWindow.restype = w.UINT
u.SetWindowPos.argtypes = [w.HWND, w.HWND, c.c_int, c.c_int, c.c_int, c.c_int, w.UINT]
u.SendMessageTimeoutW.argtypes = [w.HWND, w.UINT, w.WPARAM, w.LPARAM, w.UINT, w.UINT, c.POINTER(c.c_size_t)]
u.SendMessageTimeoutW.restype = w.LPARAM
u.IsWindow.argtypes = [w.HWND]
u.IsWindowVisible.argtypes = [w.HWND]
u.GetDC.argtypes = [w.HWND]
u.GetDC.restype = w.HDC
u.ReleaseDC.argtypes = [w.HWND, w.HDC]
g.CreateCompatibleDC.argtypes = [w.HDC]
g.CreateCompatibleDC.restype = w.HDC
g.CreateCompatibleBitmap.argtypes = [w.HDC, c.c_int, c.c_int]
g.CreateCompatibleBitmap.restype = w.HBITMAP
g.SelectObject.argtypes = [w.HDC, w.HANDLE]
g.SelectObject.restype = w.HANDLE
g.DeleteObject.argtypes = [w.HANDLE]
g.DeleteDC.argtypes = [w.HDC]
u.PrintWindow.argtypes = [w.HWND, w.HDC, w.UINT]
g.GetDIBits.argtypes = [w.HDC, w.HBITMAP, w.UINT, w.UINT, c.c_void_p, c.c_void_p, w.UINT]


class Window:
    def __init__(self, pid):
        self.pid = pid
        self.hwnd = None
        callback_type = c.WINFUNCTYPE(w.BOOL, w.HWND, w.LPARAM)

        def found(hwnd, _):
            owner = w.DWORD()
            u.GetWindowThreadProcessId(hwnd, c.byref(owner))
            if owner.value == pid:
                title = c.create_unicode_buffer(256)
                u.GetWindowTextW(hwnd, title, len(title))
                if title.value == "Loomterm Window":
                    self.hwnd = hwnd
            return True

        u.EnumWindows.argtypes = [callback_type, w.LPARAM]
        u.EnumWindows(callback_type(found), 0)
        if not self.hwnd:
            raise RuntimeError(f"no Loomterm Window HWND for owned PID {pid}")

    @classmethod
    def find(cls, pid):
        try:
            return cls(pid)
        except RuntimeError:
            return None

    def check(self):
        owner = w.DWORD()
        assert u.IsWindow(self.hwnd)
        u.GetWindowThreadProcessId(self.hwnd, c.byref(owner))
        assert owner.value == self.pid, "HWND ownership changed"

    def geometry(self):
        self.check()
        client, outer = w.RECT(), w.RECT()
        assert u.GetClientRect(self.hwnd, c.byref(client))
        assert u.GetWindowRect(self.hwnd, c.byref(outer))
        return dict(coordinate_space="physical-per-monitor-v2", pid=self.pid, hwnd=int(self.hwnd), dpi=u.GetDpiForWindow(self.hwnd),
                    visible=bool(u.IsWindowVisible(self.hwnd)),
                    client=[client.right, client.bottom],
                    window=[outer.left, outer.top, outer.right, outer.bottom])

    def resize(self, width, height):
        assert 400 <= width <= 2000 and 300 <= height <= 1400
        self.check()
        assert u.SetWindowPos(self.hwnd, None, 0, 0, width, height, 0x0002 | 0x0004 | 0x0010)
        return self.geometry()

    def close(self):
        self.check()
        result = c.c_size_t()
        assert u.SendMessageTimeoutW(self.hwnd, 0x0112, 0xF060, 0, 2, 3000, c.byref(result))

    def focus(self):
        self.check()
        u.SetForegroundWindow(self.hwnd)
        # Cross-input-queue foreground activation can complete asynchronously.
        # Wait briefly for the real foreground result; never bypass the guard.
        until = time.monotonic() + .5
        while u.GetForegroundWindow() != self.hwnd and time.monotonic() < until:
            time.sleep(.01)
        assert u.GetForegroundWindow() == self.hwnd, "owned window did not receive foreground; input skipped"

    def native_text(self, text):
        self.focus()
        events = []
        raw = text.encode("utf-16-le")
        for i in range(0, len(raw), 2):
            unit = int.from_bytes(raw[i:i+2], "little")
            events += [keyboard(0, unit, 4), keyboard(0, unit, 6)]
        inject(events)

    def native_key(self, *keys):
        self.focus()
        inject([keyboard(key, u.MapVirtualKeyW(key, 0), 0) for key in keys]
               + [keyboard(key, u.MapVirtualKeyW(key, 0), 2) for key in reversed(keys)])

    def native_click(self, x, y):
        self.focus()
        point = w.POINT(x, y)
        assert u.ClientToScreen(self.hwnd, c.byref(point))
        assert u.SetCursorPos(point.x, point.y)
        inject([mouse(2), mouse(4)])

    def native_scroll(self, delta):
        self.focus()
        inject([mouse(0x0800, delta)])

    def capture(self, path):
        from PIL import Image
        self.check()
        rect = w.RECT()
        assert u.GetWindowRect(self.hwnd, c.byref(rect))
        width, height = rect.right - rect.left, rect.bottom - rect.top
        dc = u.GetDC(self.hwnd)
        mem = g.CreateCompatibleDC(dc)
        bitmap = g.CreateCompatibleBitmap(dc, width, height)
        old = g.SelectObject(mem, bitmap)
        try:
            assert u.PrintWindow(self.hwnd, mem, 2), "PrintWindow unsupported"
            # BITMAPINFOHEADER, top-down 32-bit DIB, no color table.
            info = c.create_string_buffer(40)
            import struct
            info.raw = struct.pack("<IiiHHIIiiII", 40, width, -height, 1, 32, 0, 0, 0, 0, 0, 0)
            data = c.create_string_buffer(width * height * 4)
            assert g.GetDIBits(mem, bitmap, 0, height, data, info, 0) == height
            Image.frombytes("RGB", (width, height), data.raw, "raw", "BGRX").save(path)
        finally:
            g.SelectObject(mem, old)
            g.DeleteObject(bitmap)
            g.DeleteDC(mem)
            u.ReleaseDC(self.hwnd, dc)


class KEYBDINPUT(c.Structure):
    _fields_ = [("wVk", w.WORD), ("wScan", w.WORD), ("dwFlags", w.DWORD), ("time", w.DWORD), ("dwExtraInfo", c.c_size_t)]


class MOUSEINPUT(c.Structure):
    _fields_ = [("dx", w.LONG), ("dy", w.LONG), ("mouseData", w.DWORD), ("dwFlags", w.DWORD), ("time", w.DWORD), ("dwExtraInfo", c.c_size_t)]


class InputUnion(c.Union):
    _fields_ = [("ki", KEYBDINPUT), ("mi", MOUSEINPUT)]


class INPUT(c.Structure):
    _anonymous_ = ("data",)
    _fields_ = [("type", w.DWORD), ("data", InputUnion)]


u.GetForegroundWindow.restype = w.HWND
u.SetForegroundWindow.argtypes = [w.HWND]
u.ClientToScreen.argtypes = [w.HWND, c.POINTER(w.POINT)]
u.SendInput.argtypes = [w.UINT, c.POINTER(INPUT), c.c_int]
u.SendInput.restype = w.UINT
u.MapVirtualKeyW.argtypes = [w.UINT, w.UINT]
u.MapVirtualKeyW.restype = w.UINT


def keyboard(vk, scan, flags):
    return INPUT(type=1, data=InputUnion(ki=KEYBDINPUT(vk, scan, flags, 0, 0)))


def mouse(flags, data=0):
    return INPUT(type=0, data=InputUnion(mi=MOUSEINPUT(0, 0, data & 0xffffffff, flags, 0, 0)))


def inject(events):
    array = (INPUT * len(events))(*events)
    assert u.SendInput(len(events), array, c.sizeof(INPUT)) == len(events), "native input injection failed"


class PROCESSENTRY32(c.Structure):
    _fields_ = [("dwSize", w.DWORD), ("cntUsage", w.DWORD), ("th32ProcessID", w.DWORD),
                ("th32DefaultHeapID", c.c_size_t), ("th32ModuleID", w.DWORD),
                ("cntThreads", w.DWORD), ("th32ParentProcessID", w.DWORD),
                ("pcPriClassBase", w.LONG), ("dwFlags", w.DWORD), ("szExeFile", w.WCHAR * 260)]


def descendants(parent):
    """Read PID/parent relationships only; return bounded descendants of one owner."""
    kernel = c.WinDLL("kernel32", use_last_error=True)
    kernel.CreateToolhelp32Snapshot.argtypes = [w.DWORD, w.DWORD]
    kernel.CreateToolhelp32Snapshot.restype = w.HANDLE
    kernel.Process32FirstW.argtypes = [w.HANDLE, c.POINTER(PROCESSENTRY32)]
    kernel.Process32NextW.argtypes = [w.HANDLE, c.POINTER(PROCESSENTRY32)]
    kernel.CloseHandle.argtypes = [w.HANDLE]
    handle = kernel.CreateToolhelp32Snapshot(2, 0)
    assert handle != c.c_void_p(-1).value
    try:
        entry = PROCESSENTRY32()
        entry.dwSize = c.sizeof(entry)
        parents = {}
        available = kernel.Process32FirstW(handle, c.byref(entry))
        while available:
            parents[entry.th32ProcessID] = entry.th32ParentProcessID
            available = kernel.Process32NextW(handle, c.byref(entry))
    finally:
        kernel.CloseHandle(handle)
    found = {parent}
    for _ in range(4):
        found.update(pid for pid, owner in parents.items() if owner in found)
        assert len(found) <= 16, "unexpected test host descendant count"
    return found - {parent}
