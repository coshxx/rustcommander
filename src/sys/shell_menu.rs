//! The Windows shell context menu (Explorer's right-click menu) for items of one directory.
//!
//! `show` runs a modal menu loop, so it must be called outside of `AppCell::with`. While the menu
//! is open, `handle_menu_msg` forwards the owner-draw and submenu messages ("Senden an",
//! "Öffnen mit" …) to the shell, which fills those submenus lazily.

use std::cell::RefCell;
use std::path::{Path, PathBuf};

use windows::Win32::Foundation::{HWND, LPARAM, LRESULT, POINT, WPARAM};
use windows::Win32::System::Com::CoTaskMemFree;
use windows::Win32::UI::Input::KeyboardAndMouse::{VK_CONTROL, VK_SHIFT};
use windows::Win32::UI::Shell::Common::ITEMIDLIST;
use windows::Win32::UI::Shell::{
    CMF_CANRENAME, CMF_EXPLORE, CMF_EXTENDEDVERBS, CMF_NORMAL, CMIC_MASK_CONTROL_DOWN, CMIC_MASK_PTINVOKE,
    CMIC_MASK_SHIFT_DOWN, CMINVOKECOMMANDINFO, CMINVOKECOMMANDINFOEX, GCS_VERBW, IContextMenu, IContextMenu2,
    IContextMenu3, IShellFolder, SEE_MASK_UNICODE, SHBindToParent, SHParseDisplayName,
};
use windows::Win32::UI::WindowsAndMessaging::{
    CreatePopupMenu, DestroyMenu, SW_SHOWNORMAL, TPM_RETURNCMD, TrackPopupMenuEx, WM_DRAWITEM, WM_INITMENUPOPUP,
    WM_MEASUREITEM, WM_MENUCHAR,
};
use windows::core::{Interface, PCSTR, PCWSTR, PSTR};

use crate::files::names::to_utf16_nul;
use crate::sys::win;

/// First command ID handed to the shell; 0 means "nothing chosen".
const FIRST_ID: u32 = 1;
const LAST_ID: u32 = 0x7fff;

/// A command the app carries out itself instead of the shell.
pub enum Chosen {
    /// The shell's "Umbenennen" only works inside Explorer.
    Rename,
}

thread_local! {
    /// The menu that is open right now (for `handle_menu_msg`).
    static OPEN: RefCell<Option<IContextMenu>> = const { RefCell::new(None) };
}

/// Absolute item ID list from `SHParseDisplayName`, freed on drop.
struct Pidl(*mut ITEMIDLIST);

impl Drop for Pidl {
    fn drop(&mut self) {
        unsafe { CoTaskMemFree(Some(self.0 as *const _)) };
    }
}

/// Show the context menu for `paths` (all in one directory) at screen point `pt` and run the
/// chosen command. `extended` (Shift held) adds the extended verbs, like in Explorer.
pub fn show(hwnd: HWND, paths: &[PathBuf], pt: POINT, extended: bool) -> Result<Option<Chosen>, String> {
    let menu = context_menu(hwnd, paths).map_err(|e| format!("Kontextmenü nicht verfügbar: {}", e.message()))?;
    unsafe {
        let hmenu = CreatePopupMenu().map_err(|e| e.message())?;
        // CANRENAME: "Umbenennen" is listed; the app runs its own rename dialog for it.
        let flags = CMF_NORMAL | CMF_EXPLORE | CMF_CANRENAME | if extended { CMF_EXTENDEDVERBS } else { 0 };
        if menu.QueryContextMenu(hmenu, 0, FIRST_ID, LAST_ID, flags).is_err() {
            let _ = DestroyMenu(hmenu);
            return Err("Kontextmenü nicht verfügbar".into());
        }
        OPEN.with(|o| *o.borrow_mut() = Some(menu.clone()));
        // Left button only: the right button may still be held after a long press.
        let id = TrackPopupMenuEx(hmenu, TPM_RETURNCMD.0, pt.x, pt.y, hwnd, None).0 as u32;
        OPEN.with(|o| *o.borrow_mut() = None);
        let _ = DestroyMenu(hmenu);
        if id < FIRST_ID {
            return Ok(None);
        }
        let offset = (id - FIRST_ID) as usize;
        if verb(&menu, offset).as_deref() == Some("rename") {
            return Ok(Some(Chosen::Rename));
        }
        let dir = paths[0].parent().map(|d| to_utf16_nul(d.as_os_str()));
        let mut mask = SEE_MASK_UNICODE | CMIC_MASK_PTINVOKE;
        if win::key_down(VK_SHIFT) {
            mask |= CMIC_MASK_SHIFT_DOWN;
        }
        if win::key_down(VK_CONTROL) {
            mask |= CMIC_MASK_CONTROL_DOWN;
        }
        let info = CMINVOKECOMMANDINFOEX {
            cbSize: size_of::<CMINVOKECOMMANDINFOEX>() as u32,
            fMask: mask,
            hwnd,
            // MAKEINTRESOURCE(offset): a command by position, not by name.
            lpVerb: PCSTR(offset as *const u8),
            lpVerbW: PCWSTR(offset as *const u16),
            lpDirectoryW: dir.as_ref().map_or(PCWSTR::null(), |d| PCWSTR(d.as_ptr())),
            nShow: SW_SHOWNORMAL.0,
            ptInvoke: pt,
            ..Default::default()
        };
        // Cancelling a shell dialog (e.g. delete confirmation) is reported as an error too.
        let _ = menu.InvokeCommand(&info as *const CMINVOKECOMMANDINFOEX as *const CMINVOKECOMMANDINFO);
    }
    Ok(None)
}

/// `IContextMenu` of the items, asked from their parent folder.
fn context_menu(hwnd: HWND, paths: &[PathBuf]) -> windows::core::Result<IContextMenu> {
    let pidls = paths.iter().map(|p| parse(p)).collect::<windows::core::Result<Vec<Pidl>>>()?;
    let mut folder: Option<IShellFolder> = None;
    let mut children = Vec::with_capacity(pidls.len());
    for p in &pidls {
        let mut child: *mut ITEMIDLIST = std::ptr::null_mut();
        // `child` points into `p` (no allocation); `pidls` outlives its use.
        let f: IShellFolder = unsafe { SHBindToParent(p.0, Some(&mut child))? };
        folder.get_or_insert(f);
        children.push(child as *const ITEMIDLIST);
    }
    let folder = folder.ok_or_else(|| windows::core::Error::from_hresult(windows::Win32::Foundation::E_INVALIDARG))?;
    unsafe { folder.GetUIObjectOf(hwnd, &children, None) }
}

fn parse(path: &Path) -> windows::core::Result<Pidl> {
    let wide = to_utf16_nul(path.as_os_str());
    let mut pidl: *mut ITEMIDLIST = std::ptr::null_mut();
    unsafe { SHParseDisplayName(PCWSTR(wide.as_ptr()), None, &mut pidl, 0, None)? };
    Ok(Pidl(pidl))
}

/// Language-independent name of a menu command ("rename", "delete" …), if it has one.
fn verb(menu: &IContextMenu, offset: usize) -> Option<String> {
    let mut buf = [0u16; 64];
    unsafe {
        // Takes a `PSTR` even for the wide variant.
        let out = PSTR(buf.as_mut_ptr() as *mut u8);
        menu.GetCommandString(offset, GCS_VERBW, None, out, buf.len() as u32).ok()?;
    }
    let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    Some(String::from_utf16_lossy(&buf[..len]))
}

/// While a context menu is open: let the shell handle its submenu and owner-draw messages.
/// `None` = not such a message, or no menu open.
pub fn handle_menu_msg(msg: u32, wp: WPARAM, lp: LPARAM) -> Option<LRESULT> {
    if !matches!(msg, WM_INITMENUPOPUP | WM_DRAWITEM | WM_MEASUREITEM | WM_MENUCHAR) {
        return None;
    }
    let menu = OPEN.with(|o| o.borrow().clone())?;
    unsafe {
        if let Ok(m3) = menu.cast::<IContextMenu3>() {
            let mut result = LRESULT(0);
            m3.HandleMenuMsg2(msg, wp, lp, Some(&mut result)).ok()?;
            return Some(result);
        }
        if msg != WM_MENUCHAR
            && let Ok(m2) = menu.cast::<IContextMenu2>()
        {
            m2.HandleMenuMsg(msg, wp, lp).ok()?;
            return Some(LRESULT(if msg == WM_INITMENUPOPUP { 0 } else { 1 }));
        }
    }
    None
}
