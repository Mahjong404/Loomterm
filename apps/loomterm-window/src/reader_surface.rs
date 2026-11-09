//! Windows-only, per-launch Reader document window. It paints Reader Core's
//! visible frame; it never owns a Workspace, working PTY, or Daemon resource.
use crate::reader_host::ReaderHost;
use loomterm_reader_core::{
    view::{Input, Viewport},
    Cancellation,
};
use std::{
    error::Error,
    io,
    path::Path,
    process::{Child, ExitStatus},
    ptr, thread,
    time::Duration,
};
use windows_sys::Win32::{
    Foundation::*,
    Graphics::Gdi::*,
    System::{DataExchange::*, LibraryLoader::*, Memory::*, Ole::CF_UNICODETEXT},
    UI::{HiDpi::*, Input::KeyboardAndMouse::*, WindowsAndMessaging::*},
};

const MARGIN: i32 = 12;
const CLASS: &str = "LoomtermReaderDocumentV1";

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

struct State {
    reader: ReaderHost,
    font: HFONT,
    cell_width: i32,
    row_height: i32,
    margin: i32,
    wheel_remainder: i32,
    selection_start: Option<usize>,
    pending_frame: bool,
    released: bool,
    closed: bool,
    error: Option<String>,
}

impl State {
    fn new(path: &Path) -> Result<Box<Self>, Box<dyn Error>> {
        let mut reader = ReaderHost::new(Viewport::new(80, 25)?)?;
        reader.open(path)?;
        Ok(Box::new(Self {
            reader,
            font: ptr::null_mut(),
            cell_width: 9,
            row_height: 19,
            margin: MARGIN,
            wheel_remainder: 0,
            selection_start: None,
            pending_frame: false,
            released: false,
            closed: false,
            error: None,
        }))
    }

    unsafe fn update_font(&mut self, hwnd: HWND) {
        // All GDI handles are created, used and released on this GUI thread.
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
        let face = wide("Consolas");
        let next = unsafe {
            CreateFontW(
                -((16 * dpi as i32) / 96),
                0,
                0,
                0,
                FW_NORMAL as i32,
                0,
                0,
                0,
                DEFAULT_CHARSET as u32,
                OUT_DEFAULT_PRECIS as u32,
                CLIP_DEFAULT_PRECIS as u32,
                CLEARTYPE_QUALITY as u32,
                FIXED_PITCH as u32,
                face.as_ptr(),
            )
        };
        if next.is_null() {
            self.error = Some(io::Error::last_os_error().to_string());
            return;
        }
        let dc = unsafe { GetDC(hwnd) };
        if !dc.is_null() {
            let old = unsafe { SelectObject(dc, next as HGDIOBJ) };
            let mut extent = SIZE::default();
            let mut metrics = TEXTMETRICW::default();
            let m = wide("M");
            unsafe {
                GetTextExtentPoint32W(dc, m.as_ptr(), 1, &mut extent);
                GetTextMetricsW(dc, &mut metrics);
                SelectObject(dc, old);
                ReleaseDC(hwnd, dc);
            }
            self.cell_width = extent.cx.max(1);
            self.row_height = (metrics.tmHeight + metrics.tmExternalLeading).max(1);
        }
        self.margin = ((MARGIN * dpi as i32) / 96).max(1);
        if !self.font.is_null() {
            unsafe {
                DeleteObject(self.font as HGDIOBJ);
            }
        }
        self.font = next;
    }

    unsafe fn resize(&mut self, hwnd: HWND) {
        let mut rect = RECT::default();
        unsafe {
            GetClientRect(hwnd, &mut rect);
        }
        let width = (rect.right - rect.left - 2 * self.margin).max(self.cell_width);
        let height = (rect.bottom - rect.top - 2 * self.margin).max(self.row_height);
        let columns = (width / self.cell_width).clamp(1, 1024) as u16;
        let rows = (height / self.row_height).clamp(1, 256) as u16;
        let result = Viewport::new(columns, rows)
            .map_err(|error| error.to_string())
            .and_then(|viewport| {
                self.reader
                    .resize(viewport)
                    .map_err(|error| error.to_string())
            });
        match result {
            Ok(()) => unsafe {
                InvalidateRect(hwnd, ptr::null(), 1);
            },
            Err(error) => self.error = Some(error.to_string()),
        }
    }

    unsafe fn paint(&mut self, hwnd: HWND) {
        let mut ps = PAINTSTRUCT::default();
        let dc = unsafe { BeginPaint(hwnd, &mut ps) };
        if dc.is_null() {
            return;
        }
        let mut rect = RECT::default();
        unsafe {
            GetClientRect(hwnd, &mut rect);
            FillRect(dc, &rect, GetSysColorBrush(COLOR_WINDOW));
            SetBkMode(dc, TRANSPARENT as i32);
            SetTextColor(dc, 0x00202020);
        }
        let old = if self.font.is_null() {
            ptr::null_mut()
        } else {
            unsafe { SelectObject(dc, self.font as HGDIOBJ) }
        };
        let frame_result = self.reader.frame(&Cancellation::default());
        if frame_result.is_ok() {
            if let Some(frame) = self.reader.current_paint_frame() {
                self.pending_frame = frame.pending;
                for line in &frame.lines {
                    for cluster in &line.clusters {
                        let text: Vec<u16> = frame.text(cluster).encode_utf16().collect();
                        if text.is_empty() {
                            continue;
                        }
                        let x = self.margin + cluster.x * self.cell_width;
                        let y = self.margin + line.y * self.row_height;
                        let selected = frame.selection.as_ref().is_some_and(|range| {
                            cluster.source.bytes.start < range.end
                                && cluster.source.bytes.end > range.start
                        });
                        if selected {
                            let rect = RECT {
                                left: x,
                                top: y,
                                right: x + cluster.width as i32 * self.cell_width,
                                bottom: y + self.row_height,
                            };
                            unsafe {
                                FillRect(dc, &rect, GetSysColorBrush(COLOR_HIGHLIGHT));
                                SetTextColor(dc, GetSysColor(COLOR_HIGHLIGHTTEXT));
                            }
                        }
                        unsafe {
                            TextOutW(dc, x, y, text.as_ptr(), text.len() as i32);
                        }
                        if selected {
                            unsafe {
                                SetTextColor(dc, 0x00202020);
                            }
                        }
                    }
                }
            }
        } else {
            let core_error = self.reader.error().map(ToString::to_string);
            let message = self
                .error
                .as_deref()
                .or(core_error.as_deref())
                .unwrap_or("Loading document...");
            let text: Vec<u16> = message.encode_utf16().collect();
            unsafe {
                TextOutW(
                    dc,
                    self.margin,
                    self.margin,
                    text.as_ptr(),
                    text.len() as i32,
                );
            }
        }
        if !old.is_null() {
            unsafe {
                SelectObject(dc, old);
            }
        }
        unsafe {
            EndPaint(hwnd, &ps);
        }
    }

    unsafe fn input(&mut self, hwnd: HWND, input: Input) {
        if let Err(error) = self.reader.input(input, &Cancellation::default()) {
            self.error = Some(error.to_string());
        } else {
            unsafe {
                InvalidateRect(hwnd, ptr::null(), 1);
            }
        }
    }

    unsafe fn key(&mut self, hwnd: HWND, key: u16) -> bool {
        let ctrl = unsafe { GetKeyState(VK_CONTROL as i32) } < 0;
        match (ctrl, key) {
            (true, 0x43) => {
                unsafe {
                    self.copy_selection(hwnd);
                }
                true
            }
            (true, 0x52) | (false, VK_F5) => {
                if let Err(error) = self.reader.refresh() {
                    self.error = Some(error.to_string());
                }
                true
            }
            (false, VK_UP) => {
                unsafe {
                    self.input(hwnd, Input::ScrollRows(-1));
                }
                true
            }
            (false, VK_DOWN) => {
                unsafe {
                    self.input(hwnd, Input::ScrollRows(1));
                }
                true
            }
            (false, VK_PRIOR) => {
                unsafe {
                    self.input(hwnd, Input::Page(-1));
                }
                true
            }
            (false, VK_NEXT) => {
                unsafe {
                    self.input(hwnd, Input::Page(1));
                }
                true
            }
            (false, VK_HOME) => {
                unsafe {
                    self.input(hwnd, Input::Home);
                }
                true
            }
            (false, VK_END) => {
                unsafe {
                    self.input(hwnd, Input::End);
                }
                true
            }
            (false, VK_ESCAPE) => {
                unsafe {
                    DestroyWindow(hwnd);
                }
                true
            }
            _ => false,
        }
    }

    unsafe fn wheel(&mut self, hwnd: HWND, wparam: WPARAM) {
        self.wheel_remainder += ((wparam >> 16) as i16) as i32;
        let notches = self.wheel_remainder / 120;
        self.wheel_remainder %= 120;
        if notches != 0 {
            unsafe {
                self.input(
                    hwnd,
                    Input::Wheel(notches.clamp(i16::MIN as i32, i16::MAX as i32) as i16),
                );
            }
        }
    }

    fn hit(&mut self, lparam: LPARAM) -> Option<usize> {
        let x = (lparam as i16 as i32 - self.margin) / self.cell_width;
        let y = ((lparam >> 16) as i16 as i32 - self.margin) / self.row_height;
        if x < 0 || y < 0 {
            return None;
        }
        self.reader.frame(&Cancellation::default()).ok()??;
        self.reader
            .current_paint_frame()?
            .hit_test(x as u16, y as u16)
            .map(|c| c.source.bytes.start)
    }

    unsafe fn select_to(&mut self, hwnd: HWND, lparam: LPARAM) {
        let Some(start) = self.selection_start else {
            return;
        };
        let Some(end) = self.hit(lparam) else {
            return;
        };
        unsafe {
            self.input(hwnd, Input::Select { start, end });
        }
    }

    unsafe fn copy_selection(&self, hwnd: HWND) {
        let Some(selection) = self.reader.selection() else {
            return;
        };
        let text = wide(selection);
        if unsafe { OpenClipboard(hwnd) } == 0 {
            return;
        }
        unsafe {
            EmptyClipboard();
        }
        let bytes = text.len() * std::mem::size_of::<u16>();
        let memory = unsafe { GlobalAlloc(GMEM_MOVEABLE, bytes) };
        if !memory.is_null() {
            let dest = unsafe { GlobalLock(memory) };
            if !dest.is_null() {
                unsafe {
                    ptr::copy_nonoverlapping(text.as_ptr() as *const u8, dest as *mut u8, bytes);
                    GlobalUnlock(memory);
                }
                if unsafe { SetClipboardData(CF_UNICODETEXT as u32, memory) }.is_null() {
                    unsafe {
                        GlobalFree(memory);
                    }
                }
            } else {
                unsafe {
                    GlobalFree(memory);
                }
            }
        }
        unsafe {
            CloseClipboard();
        }
    }

    fn release(&mut self) {
        if self.released {
            return;
        }
        self.released = true;
        self.pending_frame = false;
        self.selection_start = None;
        let _ = self.reader.close();
        if !self.font.is_null() {
            unsafe {
                DeleteObject(self.font as HGDIOBJ);
            }
            self.font = ptr::null_mut();
        }
    }
}

impl Drop for State {
    fn drop(&mut self) {
        self.release();
    }
}

unsafe extern "system" fn window_proc(
    hwnd: HWND,
    message: u32,
    wparam: WPARAM,
    lparam: LPARAM,
) -> LRESULT {
    let state = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut State };
    if state.is_null() {
        return unsafe { DefWindowProcW(hwnd, message, wparam, lparam) };
    }
    let state = unsafe { &mut *state };
    match message {
        WM_PAINT => {
            unsafe {
                state.paint(hwnd);
            }
            0
        }
        WM_SIZE => {
            unsafe {
                state.resize(hwnd);
            }
            0
        }
        WM_DPICHANGED => {
            let rect = unsafe { &*(lparam as *const RECT) };
            unsafe {
                SetWindowPos(
                    hwnd,
                    ptr::null_mut(),
                    rect.left,
                    rect.top,
                    rect.right - rect.left,
                    rect.bottom - rect.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                state.update_font(hwnd);
                state.resize(hwnd);
            }
            0
        }
        WM_MOUSEWHEEL => {
            unsafe {
                state.wheel(hwnd, wparam);
            }
            0
        }
        WM_LBUTTONDOWN => {
            unsafe {
                SetFocus(hwnd);
                SetCapture(hwnd);
            }
            state.selection_start = state.hit(lparam);
            0
        }
        WM_MOUSEMOVE if state.selection_start.is_some() && (wparam & 1) != 0 => {
            unsafe {
                state.select_to(hwnd, lparam);
            }
            0
        }
        WM_LBUTTONUP => {
            unsafe {
                state.select_to(hwnd, lparam);
                ReleaseCapture();
            }
            state.selection_start = None;
            0
        }
        WM_KEYDOWN => {
            if unsafe { state.key(hwnd, wparam as u16) } {
                0
            } else {
                unsafe { DefWindowProcW(hwnd, message, wparam, lparam) }
            }
        }
        WM_CLOSE => {
            unsafe {
                DestroyWindow(hwnd);
            }
            0
        }
        WM_DESTROY => {
            state.closed = true;
            state.release();
            0
        }
        WM_NCDESTROY => unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            DefWindowProcW(hwnd, message, wparam, lparam)
        },
        _ => unsafe { DefWindowProcW(hwnd, message, wparam, lparam) },
    }
}

struct Surface {
    hwnd: HWND,
    state: Box<State>,
}

impl Surface {
    fn new(path: &Path) -> Result<Self, Box<dyn Error>> {
        let mut state = State::new(path)?;
        let class = wide(CLASS);
        let title = wide("Loomterm Reader");
        let instance = unsafe { GetModuleHandleW(ptr::null()) };
        let mut wc: WNDCLASSW = unsafe { std::mem::zeroed() };
        wc.lpfnWndProc = Some(window_proc);
        wc.hInstance = instance;
        wc.hCursor = unsafe { LoadCursorW(ptr::null_mut(), IDC_ARROW) };
        wc.hbrBackground = unsafe { GetSysColorBrush(COLOR_WINDOW) };
        wc.lpszClassName = class.as_ptr();
        if unsafe { RegisterClassW(&wc) } == 0 {
            return Err(io::Error::last_os_error().into());
        }
        let hwnd = unsafe {
            CreateWindowExW(
                0,
                class.as_ptr(),
                title.as_ptr(),
                WS_OVERLAPPEDWINDOW,
                CW_USEDEFAULT,
                CW_USEDEFAULT,
                900,
                650,
                ptr::null_mut(),
                ptr::null_mut(),
                instance,
                ptr::null(),
            )
        };
        if hwnd.is_null() {
            return Err(io::Error::last_os_error().into());
        }
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, state.as_mut() as *mut State as isize);
            state.update_font(hwnd);
            state.resize(hwnd);
            ShowWindow(hwnd, SW_SHOW);
            UpdateWindow(hwnd);
        }
        Ok(Self { hwnd, state })
    }

    fn pump(&mut self) {
        let mut msg: MSG = unsafe { std::mem::zeroed() };
        while unsafe { PeekMessageW(&mut msg, ptr::null_mut(), 0, 0, PM_REMOVE) } != 0 {
            unsafe {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
        if self.state.closed {
            return;
        }
        if self.state.pending_frame {
            self.state.pending_frame = false;
            unsafe {
                InvalidateRect(self.hwnd, ptr::null(), 1);
            }
        }
        match self.state.reader.poll() {
            Ok(Some(_)) => unsafe {
                InvalidateRect(self.hwnd, ptr::null(), 1);
            },
            Ok(None) => {}
            Err(error) => {
                self.state.error = Some(error.to_string());
                unsafe {
                    InvalidateRect(self.hwnd, ptr::null(), 1);
                }
            }
        }
    }

    fn close(&mut self) {
        if !self.state.closed {
            unsafe {
                DestroyWindow(self.hwnd);
            }
        }
    }
}

impl Drop for Surface {
    fn drop(&mut self) {
        self.close();
    }
}

pub fn run(path: &Path, child: &mut Child) -> Result<ExitStatus, Box<dyn Error>> {
    let mut surface = Surface::new(path)?;
    loop {
        if let Some(status) = child.try_wait()? {
            surface.close();
            return Ok(status);
        }
        if !surface.state.closed {
            surface.pump();
        }
        thread::sleep(Duration::from_millis(16));
    }
}
