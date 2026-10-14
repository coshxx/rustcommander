//! Safe wrappers around the Win32 calls the app logic needs.
//! Goal: `app.rs` and `panel.rs` contain no `unsafe`.

use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{FILETIME, HGLOBAL, HWND, LPARAM, SYSTEMTIME, WPARAM};
use windows::Win32::Graphics::Gdi::InvalidateRect;
use windows::Win32::Storage::FileSystem::{GetDriveTypeW, GetLogicalDrives};
use windows::Win32::System::Com::{COINIT_APARTMENTTHREADED, CoInitializeEx};
use windows::Win32::System::DataExchange::{CloseClipboard, GetClipboardData, OpenClipboard};
use windows::Win32::System::Environment::ExpandEnvironmentStringsW;
use windows::Win32::System::Memory::{GlobalLock, GlobalUnlock};
use windows::Win32::System::Time::{FileTimeToSystemTime, SystemTimeToTzSpecificLocalTime};
use windows::Win32::UI::Input::KeyboardAndMouse::{GetKeyState, VIRTUAL_KEY};
use windows::Win32::UI::Shell::ShellExecuteW;
use windows::Win32::UI::WindowsAndMessaging::{
    KillTimer, PostMessageW, SPI_GETKEYBOARDDELAY, SW_SHOWNORMAL, SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS, SetTimer,
    SystemParametersInfoW,
};
use windows::core::{PCWSTR, w};

use crate::files::names::to_utf16_nul;

pub fn invalidate(hwnd: HWND) {
    unsafe {
        let _ = InvalidateRect(Some(hwnd), None, false);
    }
}

pub fn set_timer(hwnd: HWND, id: usize, ms: u32) {
    unsafe { SetTimer(Some(hwnd), id, ms, None) };
}

pub fn kill_timer(hwnd: HWND, id: usize) {
    unsafe {
        let _ = KillTimer(Some(hwnd), id);
    }
}

/// HWND as a plain number so it can cross threads (`HWND` is not `Send`).
/// `PostMessageW` is allowed from any thread; if the window is gone by then,
/// the call merely fails.
#[derive(Clone, Copy)]
pub struct SendHwnd(isize);

impl SendHwnd {
    pub fn new(hwnd: HWND) -> SendHwnd {
        SendHwnd(hwnd.0 as isize)
    }

    pub fn post(self, msg: u32) {
        self.post_with(msg, 0);
    }

    pub fn post_with(self, msg: u32, wparam: usize) {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.0 as *mut _)), msg, WPARAM(wparam), LPARAM(0));
        }
    }
}

/// Keyboard repeat delay from the system settings (250–1000 ms), used for
/// auto-repeat of mouse actions like paging on the scrollbar track.
pub fn repeat_delay_ms() -> u32 {
    let mut v: u32 = 1;
    let _ = unsafe {
        SystemParametersInfoW(
            SPI_GETKEYBOARDDELAY,
            0,
            Some(&mut v as *mut u32 as *mut _),
            SYSTEM_PARAMETERS_INFO_UPDATE_FLAGS(0),
        )
    };
    (v.min(3) + 1) * 250
}

pub fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk.0 as i32) < 0 }
}

/// FILETIME (UTC, 100 ns ticks) to local time.
pub fn local_time(t: u64) -> Option<SYSTEMTIME> {
    let ft = FILETIME { dwLowDateTime: t as u32, dwHighDateTime: (t >> 32) as u32 };
    let (mut st, mut lt) = (SYSTEMTIME::default(), SYSTEMTIME::default());
    unsafe {
        FileTimeToSystemTime(&ft, &mut st).ok()?;
        SystemTimeToTzSpecificLocalTime(None, &st, &mut lt).ok()?;
    }
    Some(lt)
}

pub struct Drive {
    /// `C:\` without null terminator.
    pub root: [u16; 3],
    /// Result of `GetDriveTypeW` (2 = removable, 4 = network, 5 = CD, 6 = RAM disk).
    pub kind: u32,
}

pub fn drives() -> Vec<Drive> {
    let mask = unsafe { GetLogicalDrives() };
    (0..26u16)
        .filter(|i| mask & (1 << i) != 0)
        .map(|i| {
            let root = [b'A' as u16 + i, b':' as u16, b'\\' as u16, 0];
            let kind = unsafe { GetDriveTypeW(PCWSTR(root.as_ptr())) };
            Drive { root: [root[0], root[1], root[2]], kind }
        })
        .collect()
}

pub fn clipboard_text(hwnd: HWND) -> Option<Vec<u16>> {
    const CF_UNICODETEXT: u32 = 13;
    unsafe {
        OpenClipboard(Some(hwnd)).ok()?;
        let mut out = None;
        if let Ok(h) = GetClipboardData(CF_UNICODETEXT) {
            let hg = HGLOBAL(h.0);
            let p = GlobalLock(hg) as *const u16;
            if !p.is_null() {
                // SAFETY: CF_UNICODETEXT is null-terminated per the API contract, and the
                // memory stays locked and valid until GlobalUnlock.
                let mut len = 0;
                while *p.add(len) != 0 {
                    len += 1;
                }
                out = Some(std::slice::from_raw_parts(p, len).to_vec());
                let _ = GlobalUnlock(hg);
            }
        }
        let _ = CloseClipboard();
        out
    }
}

/// Expand `%VAR%` in a path; returns the input unchanged on failure.
pub fn expand_env(input: &[u16]) -> Vec<u16> {
    let mut z = input.to_vec();
    z.push(0);
    let mut buf = vec![0u16; 4096];
    let n = unsafe { ExpandEnvironmentStringsW(PCWSTR(z.as_ptr()), Some(&mut buf)) } as usize;
    if n > 0 && n <= buf.len() {
        buf.truncate(n - 1);
        buf
    } else {
        input.to_vec()
    }
}

/// Open a file with its default program. Runs on its own thread because
/// ShellExecute blocks noticeably with some handlers.
pub fn shell_open(path: PathBuf) {
    std::thread::spawn(move || {
        let file = to_utf16_nul(path.as_os_str());
        let dir = path.parent().map(|d: &Path| to_utf16_nul(d.as_os_str()));
        unsafe {
            let _ = CoInitializeEx(None, COINIT_APARTMENTTHREADED);
            ShellExecuteW(
                None,
                w!("open"),
                PCWSTR(file.as_ptr()),
                PCWSTR::null(),
                dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
                SW_SHOWNORMAL,
            );
        }
    });
}
