//! Lister windows: window class, opening, message dispatch. Each window owns its `Lister`
//! (behind an `AppCell`, freed on `WM_NCDESTROY`).

use std::cell::RefCell;
use std::path::Path;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, CreateSolidBrush, EndPaint, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::GetDpiForWindow;
use windows::Win32::UI::Input::KeyboardAndMouse::{ReleaseCapture, SetCapture, VIRTUAL_KEY};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, w};

use crate::lister::{Lister, WM_APP_SEARCH, settings};
use crate::ops::error_text;
use crate::sys::mapped::MappedFile;
use crate::theme;
use crate::window::{AppCell, dark_title_bar, show_window};

const CLASS: windows::core::PCWSTR = w!("RustCommanderLister");

thread_local! {
    /// Open lister windows, so they can be closed (and save their settings) before the app exits.
    static OPEN: RefCell<Vec<HWND>> = const { RefCell::new(Vec::new()) };
}

fn register_class() {
    thread_local!(static REGISTERED: std::cell::Cell<bool> = const { std::cell::Cell::new(false) });
    if REGISTERED.replace(true) {
        return;
    }
    unsafe {
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: GetModuleHandleW(None).unwrap_or_default().into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hbrBackground: CreateSolidBrush(COLORREF(theme::colorref(theme::DARK.bg))),
            lpszClassName: CLASS,
            ..Default::default()
        };
        RegisterClassExW(&class);
    }
}

/// Open a lister window for `path`. Errors are meant for the user (file can't be opened …).
pub fn open(path: &Path) -> Result<(), String> {
    let name = path.file_name().map(|n| n.to_string_lossy().into_owned()).unwrap_or_default();
    let file = MappedFile::open(path).map_err(|e| format!("„{name}“: {}", error_text(&e)))?;
    register_class();
    let title = HSTRING::from(format!("Lister – {}", path.display()));
    let hwnd = unsafe {
        let instance = GetModuleHandleW(None).map_err(|e| e.message())?;
        let scale = GetDpiForWindow(GetDesktopWindow()).max(96) as i32;
        CreateWindowExW(
            WINDOW_EX_STYLE::default(),
            CLASS,
            &title,
            WS_OVERLAPPEDWINDOW,
            CW_USEDEFAULT,
            CW_USEDEFAULT,
            960 * scale / 96,
            720 * scale / 96,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .map_err(|e| e.message())?
    };
    dark_title_bar(hwnd);
    let dpi = unsafe { GetDpiForWindow(hwnd) } as f32;
    let lister = match Lister::new(hwnd, file, dpi) {
        Ok(l) => l,
        Err(e) => {
            let _ = unsafe { DestroyWindow(hwnd) };
            return Err(e);
        }
    };
    let cell = Box::into_raw(Box::new(AppCell::new(lister)));
    // SAFETY: freed exactly once, on WM_NCDESTROY, after which the window gets no more messages.
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, cell as isize) };
    OPEN.with(|o| o.borrow_mut().push(hwnd));
    show_window(hwnd, settings().window);
    Ok(())
}

/// Close all lister windows (they save their settings on the way).
pub fn close_all() {
    let open = OPEN.with(|o| o.borrow().clone());
    for hwnd in open {
        unsafe { SendMessageW(hwnd, WM_CLOSE, None, None) };
    }
}

fn mouse_dips(lister: &Lister, lp: LPARAM) -> (f32, f32) {
    let x = (lp.0 & 0xffff) as i16 as f32;
    let y = ((lp.0 >> 16) & 0xffff) as i16 as f32;
    let k = 96.0 / lister.gfx.dpi;
    (x * k, y * k)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let ptr = unsafe { GetWindowLongPtrW(hwnd, GWLP_USERDATA) } as *mut AppCell<Lister>;
    let default = || unsafe { DefWindowProcW(hwnd, msg, wp, lp) };
    if ptr.is_null() {
        return default();
    }
    if msg == WM_NCDESTROY {
        unsafe {
            SetWindowLongPtrW(hwnd, GWLP_USERDATA, 0);
            // SAFETY: created by `Box::into_raw` in `open`; the pointer was just cleared, so this
            // is the only release.
            drop(Box::from_raw(ptr));
        }
        return default();
    }
    // SAFETY: valid until WM_NCDESTROY (above).
    let cell = unsafe { &*ptr };
    let done = |handled: Option<bool>| if handled == Some(true) { LRESULT(0) } else { default() };
    match msg {
        WM_PAINT => {
            let mut ps = PAINTSTRUCT::default();
            unsafe { BeginPaint(hwnd, &mut ps) };
            cell.with(|l| l.paint());
            let _ = unsafe { EndPaint(hwnd, &ps) };
            LRESULT(0)
        }
        WM_ERASEBKGND => LRESULT(1),
        WM_SIZE => {
            cell.with(|l| {
                l.gfx.resize(hwnd);
                l.layout();
            });
            LRESULT(0)
        }
        WM_DPICHANGED => {
            cell.with(|l| l.gfx.set_dpi((wp.0 & 0xffff) as f32));
            // SAFETY: for WM_DPICHANGED, lParam points to the suggested RECT.
            let r = unsafe { *(lp.0 as *const RECT) };
            // Sends WM_SIZE synchronously – hence outside of `with`.
            let _ = unsafe {
                SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                )
            };
            LRESULT(0)
        }
        WM_KEYDOWN => done(cell.with(|l| l.on_key(VIRTUAL_KEY(wp.0 as u16)))),
        WM_SYSKEYDOWN => done(cell.with(|l| l.on_syskey(VIRTUAL_KEY(wp.0 as u16)))),
        WM_SYSCOMMAND if (wp.0 & 0xfff0) as u32 == SC_KEYMENU && lp.0 == 0 => LRESULT(0),
        WM_CHAR => {
            cell.with(|l| l.on_char(wp.0 as u16));
            LRESULT(0)
        }
        WM_MOUSEWHEEL => {
            cell.with(|l| l.on_wheel(((wp.0 >> 16) & 0xffff) as i16));
            LRESULT(0)
        }
        WM_LBUTTONDOWN => {
            let capture = cell.with(|l| {
                let (x, y) = mouse_dips(l, lp);
                l.on_button_down(x, y)
            });
            // Capture changes send messages synchronously – hence outside of `with`.
            if capture == Some(true) {
                unsafe { SetCapture(hwnd) };
            }
            LRESULT(0)
        }
        WM_MOUSEMOVE => {
            cell.with(|l| {
                let (_, y) = mouse_dips(l, lp);
                l.on_mouse_move(y);
            });
            LRESULT(0)
        }
        WM_LBUTTONUP => {
            if cell.with(|l| l.on_button_up()) == Some(true) {
                let _ = unsafe { ReleaseCapture() };
            }
            LRESULT(0)
        }
        WM_CAPTURECHANGED => {
            cell.with(|l| l.cancel_drag());
            LRESULT(0)
        }
        WM_APP_SEARCH => {
            cell.with(|l| l.on_search_event());
            LRESULT(0)
        }
        WM_CLOSE => {
            cell.with(|l| l.save_settings());
            // Destroys the window and with it the `Lister` (WM_NCDESTROY) – `cell` is not used
            // after this.
            let _ = unsafe { DestroyWindow(hwnd) };
            LRESULT(0)
        }
        WM_DESTROY => {
            OPEN.with(|o| o.borrow_mut().retain(|&h| h != hwnd));
            LRESULT(0)
        }
        _ => default(),
    }
}
