//! The askpass helper: `ssh.exe` can't ask for a password itself (it has no console here), so it
//! starts `SSH_ASKPASS` = this exe with the prompt as its only argument (`RC_ASKPASS` set in
//! its environment tells us apart from a normal start). A small window in the app's look asks
//! for the password (or Yes/No for an unknown host key) and the answer goes to stdout.

use std::cell::RefCell;
use std::io::Write;

use windows::Win32::Foundation::{COLORREF, HWND, LPARAM, LRESULT, RECT, WPARAM};
use windows::Win32::Graphics::Gdi::{BeginPaint, CreateSolidBrush, EndPaint, PAINTSTRUCT};
use windows::Win32::System::LibraryLoader::GetModuleHandleW;
use windows::Win32::UI::HiDpi::{
    AdjustWindowRectExForDpi, DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2, GetDpiForSystem, GetDpiForWindow,
    SetProcessDpiAwarenessContext,
};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, SetFocus, VIRTUAL_KEY, VK_C, VK_CONTROL, VK_X};
use windows::Win32::UI::WindowsAndMessaging::*;
use windows::core::{HSTRING, w};

use crate::app::text_input::TextInput;
use crate::i18n::t;
use crate::sys::gfx::{Gfx, rect};
use crate::theme::{self, DARK};
use crate::ui::widgets::{draw_chips, draw_input};
use crate::window::{app_icon, dark_title_bar};

/// Window width and margins (DIPs).
const W: f32 = 560.0;
const PAD: f32 = 20.0;
const LINE_H: f32 = 22.0;

struct State {
    gfx: Gfx,
    lines: Vec<String>,
    /// Yes/No question instead of a password.
    confirm: bool,
    input: TextInput,
    /// Set when the window is done: the answer, or `None` = cancelled.
    answer: Option<Option<String>>,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Lets the askpass window (another process) come to the front; called before connecting.
pub fn allow_foreground() {
    unsafe {
        let _ = AllowSetForegroundWindow(ASFW_ANY);
    }
}

/// Height of the window's content (DIPs).
fn height(lines: usize, confirm: bool) -> f32 {
    PAD + 28.0 + lines as f32 * LINE_H + if confirm { 8.0 } else { 14.0 + 34.0 } + 16.0 + 28.0 + PAD - 4.0
}

/// Ask, print the answer to stdout; the exit code tells ssh whether there is one.
pub fn run(prompt: &str) -> i32 {
    unsafe {
        let _ = SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_PER_MONITOR_AWARE_V2);
    }
    let cfg = crate::config::Config::load();
    crate::i18n::set(cfg.options.language.unwrap_or_else(crate::i18n::detect));
    let confirm = prompt.contains("(yes/no") || std::env::var("SSH_ASKPASS_PROMPT").is_ok_and(|p| p == "confirm");
    let lines: Vec<String> = prompt.lines().map(|l| l.trim_end().to_string()).filter(|l| !l.is_empty()).collect();
    let Ok(gfx) = Gfx::new() else { return 1 };
    let n = lines.len();
    STATE.with(|s| {
        *s.borrow_mut() = Some(State { gfx, lines, confirm, input: TextInput::new(Vec::new()), answer: None })
    });

    let owner = std::env::var(crate::sftp::ASKPASS_OWNER_ENV)
        .ok()
        .and_then(|v| v.parse::<isize>().ok())
        .filter(|&h| h != 0)
        .map(|h| HWND(h as *mut _));
    let Some(hwnd) = create(owner, height(n, confirm)) else { return 1 };
    let mut msg = MSG::default();
    unsafe {
        while STATE.with(|s| s.borrow().as_ref().is_some_and(|s| s.answer.is_none()))
            && GetMessageW(&mut msg, None, 0, 0).as_bool()
        {
            let _ = TranslateMessage(&msg);
            DispatchMessageW(&msg);
        }
        let _ = DestroyWindow(hwnd);
    }
    let answer = STATE.with(|s| s.borrow_mut().take().and_then(|s| s.answer).flatten());
    match answer {
        Some(a) => {
            let mut out = std::io::stdout();
            let ok = writeln!(out, "{a}").and_then(|_| out.flush()).is_ok();
            if ok { 0 } else { 1 }
        }
        None => 1,
    }
}

/// The window, centered over the main window (or the screen).
fn create(owner: Option<HWND>, h_dips: f32) -> Option<HWND> {
    unsafe {
        let instance = GetModuleHandleW(None).ok()?;
        let class = WNDCLASSEXW {
            cbSize: size_of::<WNDCLASSEXW>() as u32,
            lpfnWndProc: Some(wndproc),
            hInstance: instance.into(),
            hCursor: LoadCursorW(None, IDC_ARROW).unwrap_or_default(),
            hIcon: app_icon(SM_CXICON).unwrap_or_default(),
            hIconSm: app_icon(SM_CXSMICON).unwrap_or_default(),
            hbrBackground: CreateSolidBrush(COLORREF(theme::colorref(DARK.bg))),
            lpszClassName: w!("RustCommanderAskpass"),
            ..Default::default()
        };
        RegisterClassExW(&class);
        let dpi = owner.map(|o| GetDpiForWindow(o)).filter(|&d| d > 0).unwrap_or_else(|| GetDpiForSystem());
        let k = dpi as f32 / 96.0;
        let style = WS_CAPTION | WS_SYSMENU;
        let mut r = RECT { left: 0, top: 0, right: (W * k) as i32, bottom: (h_dips * k) as i32 };
        let _ = AdjustWindowRectExForDpi(&mut r, style, false, WINDOW_EX_STYLE::default(), dpi);
        let (ww, wh) = (r.right - r.left, r.bottom - r.top);
        let mut area = RECT::default();
        let centered = match owner {
            Some(o) if GetWindowRect(o, &mut area).is_ok() => true,
            _ => SystemParametersInfoW(SPI_GETWORKAREA, 0, Some(&mut area as *mut RECT as *mut _), Default::default())
                .is_ok(),
        };
        let (x, y) = if centered {
            (area.left + (area.right - area.left - ww) / 2, area.top + (area.bottom - area.top - wh) / 3)
        } else {
            (CW_USEDEFAULT, CW_USEDEFAULT)
        };
        let title = HSTRING::from(format!("RustCommander – {}", t("askpass.title")));
        let hwnd = CreateWindowExW(
            WS_EX_TOPMOST,
            w!("RustCommanderAskpass"),
            &title,
            style,
            x,
            y,
            ww,
            wh,
            None,
            None,
            Some(instance.into()),
            None,
        )
        .ok()?;
        STATE.with(|s| {
            if let Some(s) = s.borrow_mut().as_mut() {
                s.gfx.set_dpi(dpi as f32);
            }
        });
        dark_title_bar(hwnd);
        let _ = ShowWindow(hwnd, SW_SHOWNORMAL);
        let _ = SetForegroundWindow(hwnd);
        let _ = SetFocus(Some(hwnd));
        Some(hwnd)
    }
}

fn paint(hwnd: HWND, s: &mut State) {
    if s.gfx.ensure_target(hwnd).is_err() {
        return;
    }
    let tm = &DARK;
    // Masked: as many dots as characters, same caret and selection.
    let shown =
        TextInput { text: vec!['●' as u16; s.input.text.len()], caret: s.input.caret, anchor: s.input.anchor };
    let caret_x = s.gfx.caret_x(&shown.text[..shown.caret]);
    let Some(c) = s.gfx.begin(tm.panel_bg) else { return };
    let x = PAD;
    let iw = W - 2.0 * PAD;
    let mut y = PAD - 4.0;
    let title = t(if s.confirm { "askpass.host_key" } else { "askpass.password" });
    c.text_str(title, &c.fonts.path, rect(x, y, iw, 28.0), tm.text);
    y += 28.0;
    for line in &s.lines {
        c.text_str(line, &c.fonts.name, rect(x, y, iw, LINE_H), tm.text_dim);
        y += LINE_H;
    }
    let chips: [(&str, &str); 2] = if s.confirm {
        y += 8.0;
        [("Enter", t("common.yes")), ("Esc", t("common.no"))]
    } else {
        y += 14.0;
        draw_input(&c, &s.gfx, tm, rect(x, y, iw, 34.0), &shown, Some(caret_x));
        y += 34.0;
        [("Enter", t("common.ok")), ("Esc", t("common.cancel"))]
    };
    draw_chips(&c, &s.gfx, tm, x, y + 16.0, 28.0, &chips);
    s.gfx.end();
}

extern "system" fn wndproc(hwnd: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let handled = STATE.with(|cell| {
        // Messages sent while the state is borrowed (window creation) go to DefWindowProc.
        let Ok(mut guard) = cell.try_borrow_mut() else { return false };
        let Some(s) = guard.as_mut() else { return false };
        match msg {
            WM_PAINT => {
                let mut ps = PAINTSTRUCT::default();
                unsafe { BeginPaint(hwnd, &mut ps) };
                paint(hwnd, s);
                let _ = unsafe { EndPaint(hwnd, &ps) };
            }
            WM_SIZE => s.gfx.resize(hwnd),
            WM_DPICHANGED => s.gfx.set_dpi((wp.0 & 0xffff) as f32),
            WM_CLOSE => s.answer = Some(None),
            WM_KEYDOWN => {
                let vk = VIRTUAL_KEY(wp.0 as u16);
                let ctrl = unsafe { GetKeyState(VK_CONTROL.0 as i32) } < 0;
                match vk.0 {
                    0x0D => {
                        s.answer = Some(Some(if s.confirm { "yes".into() } else { s.input.string() }));
                        s.input.text.fill(0);
                    }
                    0x1B => s.answer = Some(if s.confirm { Some("no".into()) } else { None }),
                    // No copying a password out of the field.
                    _ if ctrl && (vk == VK_C || vk == VK_X) => {}
                    _ if !s.confirm => {
                        s.input.key(vk, ctrl, hwnd);
                    }
                    _ => {}
                }
            }
            WM_CHAR => {
                let c = wp.0 as u16;
                if !s.confirm && c >= 0x20 && c != 0x7f {
                    s.input.insert(c);
                }
            }
            _ => return false,
        }
        true
    });
    if handled {
        if msg != WM_PAINT {
            unsafe {
                let _ = windows::Win32::Graphics::Gdi::InvalidateRect(Some(hwnd), None, false);
            }
        }
        return LRESULT(0);
    }
    unsafe { DefWindowProcW(hwnd, msg, wp, lp) }
}
