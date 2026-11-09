//! Windows-only, per-launch Reader document window. It paints Reader Core's
//! visible frame; it never owns a Workspace, working PTY, or Daemon resource.
use crate::reader_host::ReaderHost;
use loomterm_reader_core::{
    load_local_image,
    view::{
        Input, LogicalSize, ResourceFailure, ResourceKind, ResourceStatus, ResourceTicket, Viewport,
    },
    Cancellation, DecodedImage, ImageLimits, ImageLoadError, ImageReference, NodeKind,
};
use std::{
    collections::{BTreeMap, VecDeque},
    error::Error,
    io,
    path::{Path, PathBuf},
    process::{Child, ExitStatus},
    ptr, thread,
    thread::JoinHandle,
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
const IMAGE_CACHE_BYTES: usize = 16 * 1024 * 1024;
const IMAGE_QUEUE_LEN: usize = 16;

fn fits_image_cache(used: usize, incoming: usize) -> bool {
    incoming <= IMAGE_CACHE_BYTES.saturating_sub(used)
}

struct ImageRequest {
    generation: u64,
    ticket: ResourceTicket,
    reference: ImageReference,
}

struct ImageJob {
    request: ImageRequest,
    cancel: Cancellation,
    worker: JoinHandle<Result<DecodedImage, ImageLoadError>>,
}

struct CachedImage {
    width: u32,
    height: u32,
    // Top-down B,G,R,X bytes, composited on the Reader's white canvas.
    bgrx: Vec<u8>,
}

fn composite_bgrx(image: &DecodedImage) -> Vec<u8> {
    let mut output = Vec::with_capacity(image.rgba.len());
    for rgba in image.rgba.chunks_exact(4) {
        let alpha = u16::from(rgba[3]);
        let on_white =
            |channel: u8| ((u16::from(channel) * alpha + 255 * (255 - alpha) + 127) / 255) as u8;
        output.extend_from_slice(&[on_white(rgba[2]), on_white(rgba[1]), on_white(rgba[0]), 0]);
    }
    output
}

unsafe fn draw_image(dc: HDC, image: &CachedImage, x: i32, y: i32, width: i32, height: i32) {
    let mut bitmap: BITMAPINFO = unsafe { std::mem::zeroed() };
    bitmap.bmiHeader.biSize = std::mem::size_of::<BITMAPINFOHEADER>() as u32;
    bitmap.bmiHeader.biWidth = image.width as i32;
    bitmap.bmiHeader.biHeight = -(image.height as i32);
    bitmap.bmiHeader.biPlanes = 1;
    bitmap.bmiHeader.biBitCount = 32;
    bitmap.bmiHeader.biCompression = BI_RGB;
    unsafe {
        StretchDIBits(
            dc,
            x,
            y,
            width,
            height,
            0,
            0,
            image.width as i32,
            image.height as i32,
            image.bgrx.as_ptr().cast(),
            &bitmap,
            DIB_RGB_COLORS,
            SRCCOPY,
        );
    }
}

fn image_failure(error: &ImageLoadError) -> ResourceFailure {
    match error {
        ImageLoadError::Blocked(_) | ImageLoadError::UnsupportedFormat => {
            ResourceFailure::Unsupported
        }
        ImageLoadError::EncodedBytes
        | ImageLoadError::Dimensions
        | ImageLoadError::DecoderBudget
        | ImageLoadError::InvalidLimits => ResourceFailure::Budget,
        ImageLoadError::Source(loomterm_reader_core::ReaderError::Cancelled) => {
            ResourceFailure::Cancelled
        }
        ImageLoadError::Source(_) => ResourceFailure::Missing,
        ImageLoadError::Decode => ResourceFailure::Decode,
    }
}

fn wide(text: &str) -> Vec<u16> {
    text.encode_utf16().chain(std::iter::once(0)).collect()
}

struct State {
    source_path: PathBuf,
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
    image_cache: BTreeMap<usize, CachedImage>,
    image_cache_bytes: usize,
    image_active: Option<ImageJob>,
    image_pending: VecDeque<ImageRequest>,
}

impl State {
    fn new(path: &Path) -> Result<Box<Self>, Box<dyn Error>> {
        let mut reader = ReaderHost::new(Viewport::new(80, 25)?)?;
        reader.open(path)?;
        Ok(Box::new(Self {
            source_path: path.to_owned(),
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
            image_cache: BTreeMap::new(),
            image_cache_bytes: 0,
            image_active: None,
            image_pending: VecDeque::new(),
        }))
    }

    fn reset_images(&mut self) {
        if let Some(job) = &self.image_active {
            job.cancel.cancel();
        }
        self.image_pending.clear();
        self.image_cache.clear();
        self.image_cache_bytes = 0;
    }

    fn enqueue_images(&mut self, requests: Vec<ImageRequest>) {
        for request in requests {
            let node = request.ticket.node;
            if self.image_cache.contains_key(&node)
                || self.image_active.as_ref().is_some_and(|job| {
                    job.request.generation == request.generation && job.request.ticket.node == node
                })
                || self
                    .image_pending
                    .iter()
                    .any(|item| item.ticket.node == node)
                || self.image_pending.len() >= IMAGE_QUEUE_LEN
            {
                continue;
            }
            self.image_pending.push_back(request);
        }
        self.start_image();
    }

    fn start_image(&mut self) {
        if self.image_active.is_some() || self.released {
            return;
        }
        if let Some(request) = self.image_pending.pop_front() {
            let reference = request.reference.clone();
            let cancel = Cancellation::default();
            let worker_cancel = cancel.clone();
            let worker = thread::spawn(move || {
                load_local_image(&reference, ImageLimits::default(), &worker_cancel)
            });
            self.image_active = Some(ImageJob {
                request,
                cancel,
                worker,
            });
        }
    }

    fn poll_image(&mut self, hwnd: HWND) {
        if !self
            .image_active
            .as_ref()
            .is_some_and(|job| job.worker.is_finished())
        {
            return;
        }
        let job = self
            .image_active
            .take()
            .expect("finished image worker exists");
        let result = job.worker.join();
        if !self.released && job.request.generation == self.reader.generation() {
            let ticket = &job.request.ticket;
            match result {
                Ok(Ok(image)) => {
                    let bytes = image.rgba.len();
                    if !fits_image_cache(self.image_cache_bytes, bytes) {
                        let _ = self.reader.resource_failed(ticket, ResourceFailure::Budget);
                    } else {
                        let columns =
                            (image.width.div_ceil(self.cell_width as u32)).clamp(1, 4096) as u16;
                        let rows =
                            (image.height.div_ceil(self.row_height as u32)).clamp(1, 256) as u16;
                        let size = LogicalSize { columns, rows };
                        if let Ok(true) = self.reader.resource_size(ticket, size) {
                            let cached = CachedImage {
                                width: image.width,
                                height: image.height,
                                bgrx: composite_bgrx(&image),
                            };
                            self.image_cache_bytes += cached.bgrx.len();
                            self.image_cache.insert(ticket.node, cached);
                        }
                    }
                }
                Ok(Err(error)) => {
                    let _ = self.reader.resource_failed(ticket, image_failure(&error));
                    self.error = Some(format!("Reader image {}: {error}", ticket.node));
                }
                Err(_) => {
                    let _ = self.reader.resource_failed(ticket, ResourceFailure::Decode);
                    self.error = Some(format!("Reader image {}: worker panicked", ticket.node));
                }
            }
            unsafe {
                InvalidateRect(hwnd, ptr::null(), 1);
            }
        }
        self.start_image();
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
        let mut image_requests = Vec::new();
        if frame_result.is_ok() {
            if let Some(frame) = self.reader.current_paint_frame() {
                self.pending_frame = frame.pending;
                image_requests = frame
                    .resources
                    .iter()
                    .filter_map(|resource| {
                        if resource.kind != ResourceKind::Image {
                            return None;
                        }
                        let node = frame
                            .projection
                            .document
                            .content
                            .nodes
                            .get(resource.ticket.node)?;
                        let NodeKind::Image { reference, .. } = &node.kind else {
                            return None;
                        };
                        Some(ImageRequest {
                            generation: self.reader.generation(),
                            ticket: resource.ticket.clone(),
                            reference: reference.clone(),
                        })
                    })
                    .collect();
                for line in &frame.lines {
                    for cluster in &line.clusters {
                        let x = self.margin + cluster.x * self.cell_width;
                        let y = self.margin + line.y * self.row_height;
                        if let Some(node) = cluster.resource {
                            if let Some(image) = self.image_cache.get(&node) {
                                unsafe {
                                    draw_image(
                                        dc,
                                        image,
                                        x,
                                        y,
                                        cluster.width as i32 * self.cell_width,
                                        line.height as i32 * self.row_height,
                                    );
                                }
                                continue;
                            }
                            let label = match &frame.projection.document.content.nodes[node].kind {
                                NodeKind::Math { .. } => "[formula]",
                                _ => match self.reader.resource_status(node) {
                                    Some(ResourceStatus::Failed(ResourceFailure::Budget)) => {
                                        "[image:limit]"
                                    }
                                    Some(ResourceStatus::Failed(_)) => "[image:error]",
                                    _ => "[image:loading]",
                                },
                            };
                            let text: Vec<u16> = label.encode_utf16().collect();
                            unsafe {
                                TextOutW(dc, x, y, text.as_ptr(), text.len() as i32);
                            }
                            continue;
                        }
                        let text: Vec<u16> = frame.text(cluster).encode_utf16().collect();
                        if text.is_empty() {
                            continue;
                        }
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
        self.enqueue_images(image_requests);
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
                // A source-identical refresh must also re-read changed image files.
                // Reopen creates fresh Reader resource tickets and drops old pixels.
                self.reset_images();
                if let Err(error) = self
                    .reader
                    .close()
                    .and_then(|_| self.reader.open(&self.source_path).map(|_| ()))
                {
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
        self.reset_images();
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
        self.state.poll_image(self.hwnd);
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{Duration, Instant},
    };

    #[test]
    fn rgba_pixels_become_top_down_bgrx_on_white() {
        let image = DecodedImage {
            width: 3,
            height: 1,
            rgba: vec![255, 0, 0, 255, 0, 0, 255, 255, 0, 0, 0, 128],
        };
        assert_eq!(
            composite_bgrx(&image),
            vec![0, 0, 255, 0, 255, 0, 0, 0, 127, 127, 127, 0]
        );
    }

    #[test]
    fn image_cache_has_a_window_wide_byte_cap() {
        assert!(fits_image_cache(0, IMAGE_CACHE_BYTES));
        assert!(!fits_image_cache(1, IMAGE_CACHE_BYTES));
        assert!(!fits_image_cache(IMAGE_CACHE_BYTES, 1));
    }

    #[test]
    fn refresh_generation_does_not_deduplicate_a_stale_image_job() {
        let path = std::env::temp_dir().join(format!(
            "loomterm-reader-image-job-{}.md",
            std::process::id()
        ));
        fs::write(&path, "![image](fixture.png)").unwrap();
        let mut state = State::new(&path).unwrap();
        let deadline = Instant::now() + Duration::from_secs(3);
        while state.reader.is_loading() && Instant::now() < deadline {
            state.reader.poll().unwrap();
            thread::yield_now();
        }
        assert!(!state.reader.is_loading());
        let frame = state
            .reader
            .frame(&Cancellation::default())
            .unwrap()
            .unwrap();
        let ticket = frame.resources[0].ticket.clone();
        let reference = match &frame.projection.document.content.nodes[ticket.node].kind {
            NodeKind::Image { reference, .. } => reference.clone(),
            _ => panic!("fixture must contain an image"),
        };
        let current = state.reader.generation();
        state.image_active = Some(ImageJob {
            request: ImageRequest {
                generation: current - 1,
                ticket: ticket.clone(),
                reference: reference.clone(),
            },
            cancel: Cancellation::default(),
            worker: thread::spawn(|| Err(ImageLoadError::Decode)),
        });
        state.enqueue_images(vec![ImageRequest {
            generation: current,
            ticket,
            reference,
        }]);
        assert_eq!(state.image_pending.len(), 1);
        state.release();
        assert!(state.image_pending.is_empty());
        fs::remove_file(path).unwrap();
    }
}
