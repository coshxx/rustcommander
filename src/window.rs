//! The main window: window class, message dispatch (`wndproc`) and the `AppCell`
//! that hands the `App` to message handlers one at a time.

use std::cell::{Cell, UnsafeCell};

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE, DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, EndPaint, MONITOR_DEFAULTTONULL, MonitorFromRect, PAINTSTRUCT, ScreenToClient,
};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForSystem;
use windows::Win32::UI::Input::KeyboardAndMouse::{
    ReleaseCapture, SetCapture, SetFocus, TME_LEAVE, TRACKMOUSEEVENT, TrackMouseEvent, VIRTUAL_KEY,
};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, PCWSTR, w};

use crate::app::App;
use crate::{app, lister, panel, theme};

/// Resource ID of the icon in `res/app.rc`.
const ICON_ID: u16 = 1;

/// "RustCommander 0.1.0", plus "(Debug)" in debug builds.
fn title() -> HSTRING {
    let debug = if cfg!(debug_assertions) { " (Debug)" } else { "" };
    HSTRING::from(format!("RustCommander {}{debug}", env!("CARGO_PKG_VERSION")))
}

/// The app icon in the system's size for `metric` (`SM_CXICON` / `SM_CXSMICON`); `None` in a
/// build without the resource.
pub(crate) fn app_icon(metric: SYSTEM_METRICS_INDEX) -> Option<HICON> {
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let s = GetSystemMetrics(metric);
        let icon = LoadImageW(
            Some(instance.into()),
            PCWSTR(ICON_ID as usize as *const u16),
            IMAGE_ICON,
            s,
            s,
            LR_DEFAULTCOLOR,
        )
        .ok()?;
        Some(HICON(icon.0))
    }
}

/// The app behind a lock. Win32 delivers some messages synchronously while we are still
/// inside a handler (dialogs, `SetWindowPos`, `DestroyWindow`, modal loops). Without the
/// lock that would create two `&mut App` at once – undefined behavior. Instead, nested
/// access is detected: debug builds panic (to find the bug), release builds pass the
/// message on to `DefWindowProcW`.
/// Lister windows use the same cell for their state (`AppCell<Lister>`).
pub(crate) struct AppCell<T = App> {
    busy: Cell<bool>,
    app: UnsafeCell<T>,
}

impl<T> AppCell<T> {
    pub(crate) fn new(app: T) -> AppCell<T> {
        AppCell { busy: Cell::new(false), app: UnsafeCell::new(app) }
    }

    /// `None` if the app is already in use.
    pub(crate) fn with<R>(&self, f: impl FnOnce(&mut T) -> R) -> Option<R> {
        if self.busy.replace(true) {
            debug_assert!(false, "nested access to App (reentrancy in wndproc)");
            return None;
        }
        struct Release<'a>(&'a Cell<bool>);
        impl Drop for Release<'_> {
            fn drop(&mut self) {
                self.0.set(false);
            }
        }
        let _release = Release(&self.busy);
        // SAFETY: `busy` guarantees no second reference exists; `AppCell` is not `Sync`,
        // so only the UI thread accesses it.
        Some(f(unsafe { &mut *self.app.get() }))
    }
}

/// Register the window class and create the (still hidden) main window.
pub(crate) fn create_window() -> HWND {
    unsafe {
        let instance = GetModuleHandleW(None).expect("GetModuleHandleW");
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: app_icon(SM_CXICON).unwrap_or_default(),
            hIconSm: app_icon(SM_CXSMICON).unwrap_or_default(),
            // Dark GDI background: no white flash before the first frame.
            hbrBackground: CreateSolidBrush(COLORREF(theme::colorref(theme::DARK.bg))),
            lpszClassName: w!("RustCommanderWnd"),
            ..Default::default()
        };
        RegisterClassExW(&class);

        let scale = GetDpiForSystem() as i32;
        let (w, h) = (1200 * scale / 96, 760 * scale / 96);
        let hwnd = CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            w!("RustCommanderWnd"),
            &title(),
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            w,
            h,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .expect("CreateWindowExW");

        dark_title_bar(hwnd);
        hwnd
    }
}

/// Dark title bar and border, matching the theme.
pub(crate) fn dark_title_bar(hwnd: HWND) {
    let set = |attr, v: u32| {
        let _ = unsafe { DwmSetWindowAttribute(hwnd, attr, &v as *const u32 as *const _, 4) };
    };
    set(DWMWA_USE_IMMERSIVE_DARK_MODE, 1);
    set(DWMWA_CAPTION_COLOR, theme::colorref(0x16181d));
    set(DWMWA_BORDER_COLOR, theme::colorref(0x2b2e36));
    set(DWMWA_TEXT_COLOR, theme::colorref(0xd8dbe2));
}

/// Show the window at a saved position (normal rectangle + maximized), or where Windows puts
/// it if there is none or it is off-screen.
pub(crate) fn show_window(hwnd: HWND, placement: Option<([i32; 4], bool)>) {
    unsafe {
        if let Some((r, max)) = placement {
            let rc = RECT { left: r[0], top: r[1], right: r[2], bottom: r[3] };
            if !MonitorFromRect(&rc, MONITOR_DEFAULTTONULL).is_invalid() {
                let wp = WINDOWPLACEMENT {
                    length: size_of::<WINDOWPLACEMENT>() as u32,
                    showCmd: if max { SW_SHOWMAXIMIZED.0 as u32 } else { SW_SHOWNORMAL.0 as u32 },
                    rcNormalPosition: rc,
                    ..Default::default()
                };
                if SetWindowPlacement(hwnd, &wp).is_ok() {
                    return;
                }
            }
        }
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
    }
}

/// Normal (restored) position and maximized state, for saving.
pub fn window_placement(hwnd: HWND) -> Option<([i32; 4], bool)> {
    let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    unsafe { GetWindowPlacement(hwnd, &mut wp).ok()? };
    let r = wp.rcNormalPosition;
    Some(([r.left, r.top, r.right, r.bottom], wp.showCmd == SW_SHOWMAXIMIZED.0 as u32))
}

/// Lives in the `Win32_UI_Controls` feature of the `windows` crate; not worth the extra feature.
pub(crate) const WM_MOUSELEAVE: u32 = 0x02A3;

/// Mouse position from `lParam`, in DIPs.
pub(crate) fn mouse_dips(app: &App, lp: LPARAM) -> (f32, f32) {
    let x = (lp.0 & 0xffff) as i16 as f32;
    let y = ((lp.0 >> 16) & 0xffff) as i16 as f32;
    let k = 96.0 / app.gfx.dpi;
    (x * k, y * k)
}

/// Message dispatch: forwards each message to the matching `App` handler.
extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *const AppCell;
    if ptr.is_null() {
        return unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    }
    // SAFETY: see `main` – the pointer lives as long as the window exists.
    let cell = unsafe { &*ptr };
    if let Some(result) = mouse_message(cell, hwnd, msg, wp, lp) {
        return result;
    }
    let default = || unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    let done = |handled: Option<bool>| if handled == Some(true) { LRESULT(0) } else { default() };

    match msg {
        WM_PAINT => {
            // Always BeginPaint/EndPaint, otherwise WM_PAINT keeps coming back.
            let mut ps = PAINTSTRUCT::default();
            unsafe { BeginPaint(hwnd, &mut ps) };
            cell.with(|app| app.paint());
            let _ = unsafe { EndPaint(hwnd, &ps) };
            LRESULT(0)
        }
        // We draw everything ourselves; GDI must not paint over it.
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            cell.with(|app| {
                app.gfx.resize(hwnd);
                app.layout();
            });
            LRESULT(0)
        }
        WM_DPICHANGED => {
            dpi_changed(cell, hwnd, wp, lp);
            LRESULT(0)
        }
        WM_KEYDOWN => done(cell.with(|app| app.on_key(VIRTUAL_KEY(wp.0 as u16)))),
        WM_SYSKEYDOWN => done(cell.with(|app| app.on_syskey(VIRTUAL_KEY(wp.0 as u16)))),
        // Alt alone must not enter the (non-existent) menu.
        WM_SYSCOMMAND if (wp.0 & 0xfff0) as u32 == SC_KEYMENU && lp.0 == 0 => LRESULT(0),
        WM_CHAR => done(cell.with(|app| {
            app.on_char(wp.0 as u16);
            true
        })),
        // Alt+Shift+Enter would beep otherwise (no menu to handle the character).
        WM_SYSCHAR if wp.0 == 0x0D => LRESULT(0),
        WM_SYSCHAR => done(cell.with(|app| app.on_syschar(wp.0 as u16))),
        panel::WM_APP_SIZE => {
            cell.with(|app| app.on_size_done());
            LRESULT(0)
        }
        panel::WM_APP_LOADED => {
            cell.with(|app| app.on_loaded());
            LRESULT(0)
        }
        WM_TIMER => {
            cell.with(|app| app.on_timer(wp.0));
            LRESULT(0)
        }
        app::WM_APP_JOB => {
            cell.with(|app| app.on_job_event());
            LRESULT(0)
        }
        app::WM_APP_DIRCHANGED => {
            cell.with(|app| app.on_dir_changed(wp.0));
            LRESULT(0)
        }
        app::WM_APP_VIEW => {
            open_listers(cell);
            LRESULT(0)
        }
        WM_CLOSE => {
            // Listers first: they save their settings, which go into the config below.
            lister::close_all();
            cell.with(|app| {
                if !app.timing.bench {
                    app.config(hwnd).save();
                }
            });
            // Sends WM_DESTROY synchronously – hence outside of `with`.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            unsafe { PostQuitMessage(0) };
            LRESULT(0)
        }
        _ => default(),
    }
}

/// `WM_APP_VIEW`: open the files the app asked to view. Creating a window sends messages
/// synchronously (activation, focus), hence outside of `with`.
fn open_listers(cell: &AppCell) {
    let paths = cell.with(|app| std::mem::take(&mut app.view_requests)).unwrap_or_default();
    for path in paths {
        if let Err(e) = lister::open(&path) {
            cell.with(|app| app.notify(e, app::Tone::Error));
        }
    }
}

/// Moving to a monitor with another DPI: new fonts/scale, then the size Windows suggests.
fn dpi_changed(cell: &AppCell, hwnd: HWND, wp: WPARAM, lp: LPARAM) {
    cell.with(|app| app.gfx.set_dpi((wp.0 & 0xffff) as f32));
    // SAFETY: for WM_DPICHANGED, lParam points to the suggested RECT per the API.
    let r = unsafe { *(lp.0 as *const RECT) };
    // Sends WM_SIZE synchronously – hence outside of `with`.
    let _ = unsafe {
        SetWindowPos(hwnd, None, r.left, r.top, r.right - r.left, r.bottom - r.top, SWP_NOZORDER | SWP_NOACTIVATE)
    };
}

/// Mouse messages; `None` if `msg` is not one.
fn mouse_message(cell: &AppCell, hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    match msg {
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK | WM_RBUTTONDOWN | WM_RBUTTONDBLCLK => {
            let _ = unsafe { SetFocus(Some(hwnd)) };
            let right = matches!(msg, WM_RBUTTONDOWN | WM_RBUTTONDBLCLK);
            let capture = cell.with(|app| {
                let (x, y) = mouse_dips(app, lp);
                app.on_button_down(x, y, right, msg == WM_LBUTTONDBLCLK)
            });
            // Capture changes send messages synchronously – hence outside of `with`.
            if capture == Some(true) {
                unsafe { SetCapture(hwnd) };
            }
        }
        // Handled (no DefWindowProc) so a right click doesn't produce WM_CONTEXTMENU.
        WM_LBUTTONUP | WM_RBUTTONUP => {
            let ended = cell.with(|app| {
                let (x, y) = mouse_dips(app, lp);
                app.on_button_up(x, y, msg == WM_RBUTTONUP)
            });
            if ended == Some(true) {
                let _ = unsafe { ReleaseCapture() };
            }
        }
        WM_MOUSEMOVE => {
            // Ask for WM_MOUSELEAVE so the scrollbar hover ends when the mouse leaves the window.
            let mut tme = TRACKMOUSEEVENT {
                cbSize: size_of::<TRACKMOUSEEVENT>() as u32,
                dwFlags: TME_LEAVE,
                hwndTrack: hwnd,
                dwHoverTime: 0,
            };
            let _ = unsafe { TrackMouseEvent(&mut tme) };
            cell.with(|app| {
                let (x, y) = mouse_dips(app, lp);
                app.on_mouse_move(x, y);
            });
        }
        WM_MOUSELEAVE => {
            cell.with(|app| app.on_mouse_leave());
        }
        WM_CAPTURECHANGED => {
            cell.with(|app| app.cancel_drag());
        }
        WM_MOUSEWHEEL => {
            // Wheel positions are in screen coordinates.
            let mut pt = POINT { x: (lp.0 & 0xffff) as i16 as i32, y: ((lp.0 >> 16) & 0xffff) as i16 as i32 };
            let _ = unsafe { ScreenToClient(hwnd, &mut pt) };
            let handled = cell.with(|app| {
                app.on_wheel(pt.x as f32 * 96.0 / app.gfx.dpi, ((wp.0 >> 16) & 0xffff) as i16);
            });
            if handled.is_none() {
                return Some(unsafe { DefWindowProcW(hwnd, msg, wp, lp) });
            }
        }
        _ => return None,
    }
    Some(LRESULT(0))
}
