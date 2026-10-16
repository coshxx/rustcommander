//! RustCommander: a fast dual-pane file manager for Windows.
//!
//! Startup: parse arguments, load the config, create the window, start the message loop.
//! See ARCHITECTURE.md for how the modules fit together.

#![windows_subsystem = "windows"]

mod app;
mod config;
mod files;
mod i18n;
mod lister;
mod ops;
mod panel;
mod sys;
mod theme;
mod timing;
mod ui;
mod window;

use windows::Win32::System::Console::{ATTACH_PARENT_PROCESS, AttachConsole};
use windows::Win32::System::Ole::OleInitialize;
use windows::Win32::UI::HiDpi::{
    DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForWindow, SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::WindowsAndMessaging::*;

use crate::app::App;
use crate::config::Config;
use crate::timing::Timing;
use crate::window::{AppCell, create_window, show_window};

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
        // The shell context menu needs OLE on this thread (copy/cut use the OLE clipboard).
        let _ = OleInitialize(None);
    }
    let cfg = Config::load();
    i18n::set(cfg.options.language.unwrap_or_else(i18n::detect));
    lister::set_settings(cfg.lister.clone());
    let hwnd = create_window();
    timing.mark("window created");

    let cell = Box::into_raw(Box::new(AppCell::new(App::new(hwnd, &cfg, timing))));
    // SAFETY: `cell` comes from `Box::into_raw` and is only freed after the message loop –
    // by then the window is destroyed and `wndproc` never sees the pointer again.
    let cell_ref = unsafe { &*cell };
    cell_ref.with(|app| {
        app.gfx.set_dpi(unsafe { GetDpiForWindow(hwnd) } as f32);
    });
    unsafe { SetWindowLongPtrW(hwnd, GWLP_USERDATA, cell as isize) };
    window::accept_drops(hwnd);
    cell_ref.with(|app| app.timing.mark("before ShowWindow"));
    show_window(hwnd, cfg.window);
    // Leftovers from files opened out of archives in earlier sessions.
    std::thread::spawn(files::temp::sweep_old);
    cell_ref.with(|app| app.timing.mark("after ShowWindow"));

    let mut msg = MSG::default();
    unsafe {
        while GetMessageW(&mut msg, None, 0, 0).as_bool() {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
    }
    // SAFETY: see above; the loop has ended and the window is destroyed.
    let cell = unsafe { Box::from_raw(cell) };
    cell.with(|app| app.timing.flush());
    files::temp::cleanup_session();
}
