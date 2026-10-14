//! Sichere Hüllen um die Win32-Aufrufe, die die App-Logik braucht.
//! Ziel: `app.rs` und `panel.rs` kommen ohne `unsafe` aus.

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
use windows::Win32::UI::WindowsAndMessaging::{KillTimer, PostMessageW, SW_SHOWNORMAL, SetTimer};
use windows::core::{PCWSTR, w};

use crate::fsx::to_wide_z;

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

/// HWND als Zahl, damit es in andere Threads darf (`HWND` ist nicht `Send`).
/// `PostMessageW` ist aus jedem Thread erlaubt; ein inzwischen zerstörtes Fenster
/// lässt den Aufruf nur fehlschlagen.
#[derive(Clone, Copy)]
pub struct SendHwnd(isize);

impl SendHwnd {
    pub fn new(hwnd: HWND) -> SendHwnd {
        SendHwnd(hwnd.0 as isize)
    }

    pub fn post(self, msg: u32) {
        unsafe {
            let _ = PostMessageW(Some(HWND(self.0 as *mut _)), msg, WPARAM(0), LPARAM(0));
        }
    }
}

pub fn key_down(vk: VIRTUAL_KEY) -> bool {
    unsafe { GetKeyState(vk.0 as i32) < 0 }
}

/// FILETIME (UTC, 100-ns-Ticks) in lokale Zeit.
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
    /// `C:\` ohne Nullterminator.
    pub root: [u16; 3],
    /// Rückgabe von `GetDriveTypeW` (2 = Wechseldatenträger, 4 = Netz, 5 = CD, 6 = RAM).
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
                // SAFETY: CF_UNICODETEXT ist laut API-Vertrag nullterminiert, und der
                // Speicher bleibt bis GlobalUnlock gesperrt und gültig.
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

/// `%VAR%` in einem Pfad auflösen; bei Fehler unverändert.
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

/// Datei mit dem Standardprogramm öffnen. Läuft im eigenen Thread, weil
/// ShellExecute bei manchen Handlern spürbar blockiert.
pub fn shell_open(path: PathBuf) {
    std::thread::spawn(move || {
        let file = to_wide_z(path.as_os_str());
        let dir = path.parent().map(|d: &Path| to_wide_z(d.as_os_str()));
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
