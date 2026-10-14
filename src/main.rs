#![windows_subsystem = "windows"]

mod app;
mod config;
mod fsx;
mod gfx;
mod panel;
mod theme;
mod timing;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, POINT, RECT, WPARAM};
use windows::Win32::Graphics::Dwm::{
    DWMWA_BORDER_COLOR, DWMWA_CAPTION_COLOR, DWMWA_TEXT_COLOR, DWMWA_USE_IMMERSIVE_DARK_MODE,
    DwmSetWindowAttribute,
};
use windows::Win32::Graphics::Gdi::{
    BeginPaint, CreateSolidBrush, EndPaint, MONITOR_DEFAULTTONULL, MonitorFromRect, PAINTSTRUCT,
    ScreenToClient,
};
use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{SetFocus, VIRTUAL_KEY};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::w;

use app::App;
use config::Config;
use timing::Timing;

const TITLE: windows::core::PCWSTR = w!("RustCommander");

fn main() {
    let args: Vec<String> = std::env::args().skip(1).collect();
    let bench = args.iter().any(|a| a == "--bench");
    let timed = bench || args.iter().any(|a| a == "--timing");
    if timed {
        unsafe {
            let _ = AttachConsole(ATTACH_PARENT_PROCESS);
        }
    }
    let mut timing = Timing::new(timed, bench);
    timing.mark("main");

    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let cfg = Config::load();
    let hwnd = create_window();
    timing.mark("Fenster erzeugt");

    let app = Box::into_raw(Box::new(App::new(hwnd, &cfg, timing)));
    unsafe {
        (*app).gfx.set_dpi(GetDpiForWindow(hwnd) as f32);
        SetWindowLongPtrW(hwnd, GWLP_USERDATA, app as isize);
        (*app).timing.mark("vor ShowWindow");
        show_window(hwnd, &cfg);
        (*app).timing.mark("nach ShowWindow");

        let mut msg = MSG::default();
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let mut app = Box::from_raw(app);
        app.timing.flush();
    }
}

fn create_window() -> HWND {
    unsafe {
        let instance = GetModuleHandleW(None).expect("GetModuleHandleW");
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            style: CS_DBLCLKS,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            // Dunkler GDI-Hintergrund: kein weißes Aufblitzen vor dem ersten Frame.
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
            TITLE,
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

        let set = |attr, v: u32| {
            let _ = DwmSetWindowAttribute(hwnd, attr, &v as *const u32 as *const _, 4);
        };
        set(DWMWA_USE_IMMERSIVE_DARK_MODE, 1);
        set(DWMWA_CAPTION_COLOR, theme::colorref(0x16181d));
        set(DWMWA_BORDER_COLOR, theme::colorref(0x2b2e36));
        set(DWMWA_TEXT_COLOR, theme::colorref(0xd8dbe2));
        hwnd
    }
}

fn show_window(hwnd: HWND, cfg: &Config) {
    unsafe {
        if let Some((r, max)) = cfg.window {
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

pub fn window_placement(hwnd: HWND) -> Option<([i32; 4], bool)> {
    let mut wp = WINDOWPLACEMENT { length: size_of::<WINDOWPLACEMENT>() as u32, ..Default::default() };
    unsafe { GetWindowPlacement(hwnd, &mut wp).ok()? };
    let r = wp.rcNormalPosition;
    Some(([r.left, r.top, r.right, r.bottom], wp.showCmd == SW_SHOWMAXIMIZED.0 as u32))
}

fn mouse_dips(app: &App, lp: LPARAM) -> (f32, f32) {
    let x = (lp.0 & 0xffff) as i16 as f32;
    let y = ((lp.0 >> 16) & 0xffff) as i16 as f32;
    let k = 96.0 / app.gfx.dpi;
    (x * k, y * k)
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    unsafe {
        let ptr = GetWindowLongPtrW(hwnd, GWLP_USERDATA) as *mut App;
        if ptr.is_null() {
            return DefWindowProcW(hwnd, msg, wp, lp);
        }
        // Achtung: Aufrufe, die synchron neue Nachrichten auslösen (SetWindowPos,
        // DestroyWindow), passieren erst, nachdem die `&mut App`-Referenz nicht mehr lebt.
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                BeginPaint(hwnd, &mut ps);
                (*ptr).paint();
                let _ = EndPaint(hwnd, &ps);
                LRESULT(0)
            }
            WM_ERASEBKGND => {
                // Sobald wir selbst zeichnen können, nicht mehr von GDI übermalen lassen.
                LRESULT(1)
            }
            WM_SIZE => {
                let app = &mut *ptr;
                app.gfx.resize(hwnd);
                app.layout();
                LRESULT(0)
            }
            WM_DPICHANGED => {
                (*ptr).gfx.set_dpi((wp.0 & 0xffff) as f32);
                let r = &*(lp.0 as *const RECT);
                let _ = SetWindowPos(
                    hwnd,
                    None,
                    r.left,
                    r.top,
                    r.right - r.left,
                    r.bottom - r.top,
                    SWP_NOZORDER | SWP_NOACTIVATE,
                );
                LRESULT(0)
            }
            WM_KEYDOWN => {
                if (*ptr).on_key(VIRTUAL_KEY(wp.0 as u16)) {
                    LRESULT(0)
                } else {
                    DefWindowProcW(hwnd, msg, wp, lp)
                }
            }
            WM_SYSKEYDOWN => {
                if (*ptr).on_syskey(VIRTUAL_KEY(wp.0 as u16)) {
                    LRESULT(0)
                } else {
                    DefWindowProcW(hwnd, msg, wp, lp)
                }
            }
            WM_SYSCOMMAND if (wp.0 & 0xfff0) as u32 == SC_KEYMENU && lp.0 == 0 => {
                // Alt alleine soll nicht ins (nicht vorhandene) Menü springen.
                LRESULT(0)
            }
            WM_CHAR => {
                (*ptr).on_char(wp.0 as u16);
                LRESULT(0)
            }
            WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
                let _ = SetFocus(Some(hwnd));
                let app = &mut *ptr;
                let (x, y) = mouse_dips(app, lp);
                app.on_click(x, y, msg == WM_LBUTTONDBLCLK);
                LRESULT(0)
            }
            WM_MOUSEWHEEL => {
                let mut pt = POINT {
                    x: (lp.0 & 0xffff) as i16 as i32,
                    y: ((lp.0 >> 16) & 0xffff) as i16 as i32,
                };
                let _ = ScreenToClient(hwnd, &mut pt);
                let app = &mut *ptr;
                app.on_wheel(pt.x as f32 * 96.0 / app.gfx.dpi, ((wp.0 >> 16) & 0xffff) as i16);
                LRESULT(0)
            }
            panel::WM_APP_LOADED => {
                (*ptr).on_loaded();
                LRESULT(0)
            }
            WM_TIMER => {
                (*ptr).on_timer();
                LRESULT(0)
            }
            WM_CLOSE => {
                {
                    let app = &mut *ptr;
                    if !app.timing.bench {
                        app.config(hwnd).save();
                    }
                }
                let _ = DestroyWindow(hwnd);
                LRESULT(0)
            }
            WM_DESTROY => {
                PostQuitMessage(0);
                LRESULT(0)
            }
            _ => DefWindowProcW(hwnd, msg, wp, lp),
        }
    }
}
