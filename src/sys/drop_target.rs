//! The window as an OLE drop target: files dragged onto it from the panels themselves or from
//! other programs (Explorer, desktop). Only real files (`CF_HDROP`) are taken.
//!
//! OLE calls the target from the message loop (or from our own drag loop, `shell_menu::drag_out`),
//! never while the app is borrowed, so the handler may use `AppCell::with`. The shell's drag image
//! is shown over the window with `IDropTargetHelper`, like in Explorer.
//!
//! The source is always told "copied" (or nothing): it never deletes anything itself, a move is
//! done by the app after its dialog.

use std::cell::RefCell;
use std::ffi::OsString;
use std::os::windows::ffi::OsStringExt;
use std::path::PathBuf;

use windows::Win32::Foundation::{HWND, POINT, POINTL};
use windows::Win32::Graphics::Gdi::ScreenToClient;
use windows::Win32::System::Com::{
    CLSCTX_INPROC_SERVER, CoCreateInstance, DVASPECT_CONTENT, FORMATETC, IDataObject, TYMED_HGLOBAL,
};
use windows::Win32::System::Ole::{
    CF_HDROP, DROPEFFECT, DROPEFFECT_COPY, DROPEFFECT_MOVE, DROPEFFECT_NONE, IDropTarget, IDropTarget_Impl,
    RegisterDragDrop, ReleaseStgMedium, RevokeDragDrop,
};
use windows::Win32::System::SystemServices::{MK_SHIFT, MODIFIERKEYS_FLAGS};
use windows::Win32::UI::Shell::{CLSID_DragDropHelper, DragQueryFileW, HDROP, IDropTargetHelper};
use windows::core::{Ref, implement};

/// What the handler is told; positions are client pixels.
pub enum DropEvent<'a> {
    /// The mouse is over the window with `paths`: may they be dropped here?
    Over { paths: &'a [PathBuf], at: POINT },
    /// The drag left the window or was cancelled.
    Leave,
    /// Dropped: take them (returns whether they were taken). `mov`: Shift is held.
    Drop { paths: Vec<PathBuf>, at: POINT, mov: bool },
}

pub type Handler = Box<dyn Fn(DropEvent) -> bool>;

/// Make `hwnd` a drop target (OLE must be initialized on this thread).
pub fn register(hwnd: HWND, handler: Handler) {
    let helper = unsafe { CoCreateInstance(&CLSID_DragDropHelper, None, CLSCTX_INPROC_SERVER) }.ok();
    let target: IDropTarget = DropTarget { hwnd, handler, helper, paths: RefCell::new(None) }.into();
    let _ = unsafe { RegisterDragDrop(hwnd, &target) };
}

/// Before the window is destroyed.
pub fn revoke(hwnd: HWND) {
    let _ = unsafe { RevokeDragDrop(hwnd) };
}

#[implement(IDropTarget)]
struct DropTarget {
    hwnd: HWND,
    handler: Handler,
    helper: Option<IDropTargetHelper>,
    /// The files of the drag in progress; `None` if it carries none.
    paths: RefCell<Option<Vec<PathBuf>>>,
}

impl DropTarget {
    /// Ask the handler and pick the effect: copy, or move with Shift; none if the source
    /// doesn't allow it.
    fn effect(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, allowed: DROPEFFECT) -> DROPEFFECT {
        let paths = self.paths.borrow();
        let Some(paths) = paths.as_deref() else { return DROPEFFECT_NONE };
        if !(self.handler)(DropEvent::Over { paths, at: self.client(pt) }) {
            return DROPEFFECT_NONE;
        }
        let effect = if keys.0 & MK_SHIFT.0 != 0 { DROPEFFECT_MOVE } else { DROPEFFECT_COPY };
        if allowed.0 & effect.0 != 0 { effect } else { DROPEFFECT_NONE }
    }

    fn client(&self, pt: &POINTL) -> POINT {
        let mut p = POINT { x: pt.x, y: pt.y };
        let _ = unsafe { ScreenToClient(self.hwnd, &mut p) };
        p
    }
}

impl IDropTarget_Impl for DropTarget_Impl {
    fn DragEnter(
        &self,
        data: Ref<IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        *self.paths.borrow_mut() = data.as_ref().and_then(file_paths);
        // SAFETY: OLE passes a valid pointer to the allowed effects, to be replaced by ours.
        unsafe {
            *effect = self.effect(keys, pt, *effect);
            if let Some(h) = &self.helper {
                let _ = h.DragEnter(self.hwnd, data.as_ref(), &POINT { x: pt.x, y: pt.y }, *effect);
            }
        }
        Ok(())
    }

    fn DragOver(&self, keys: MODIFIERKEYS_FLAGS, pt: &POINTL, effect: *mut DROPEFFECT) -> windows::core::Result<()> {
        unsafe {
            *effect = self.effect(keys, pt, *effect);
            if let Some(h) = &self.helper {
                let _ = h.DragOver(&POINT { x: pt.x, y: pt.y }, *effect);
            }
        }
        Ok(())
    }

    fn DragLeave(&self) -> windows::core::Result<()> {
        *self.paths.borrow_mut() = None;
        (self.handler)(DropEvent::Leave);
        if let Some(h) = &self.helper {
            let _ = unsafe { h.DragLeave() };
        }
        Ok(())
    }

    fn Drop(
        &self,
        data: Ref<IDataObject>,
        keys: MODIFIERKEYS_FLAGS,
        pt: &POINTL,
        effect: *mut DROPEFFECT,
    ) -> windows::core::Result<()> {
        let allowed = unsafe { *effect };
        let chosen = self.effect(keys, pt, allowed);
        let paths = self.paths.borrow_mut().take();
        let taken = match paths {
            Some(paths) if chosen != DROPEFFECT_NONE => {
                let mov = keys.0 & MK_SHIFT.0 != 0;
                (self.handler)(DropEvent::Drop { paths, at: self.client(pt), mov })
            }
            _ => {
                (self.handler)(DropEvent::Leave);
                false
            }
        };
        // Never "moved": the source must not delete anything (see the module docs).
        let result = if taken && allowed.0 & DROPEFFECT_COPY.0 != 0 { DROPEFFECT_COPY } else { DROPEFFECT_NONE };
        unsafe {
            *effect = result;
            if let Some(h) = &self.helper {
                let _ = h.Drop(data.as_ref(), &POINT { x: pt.x, y: pt.y }, chosen);
            }
        }
        Ok(())
    }
}

/// The file paths in `data` (`CF_HDROP`); `None` if it has none (text, virtual files …).
fn file_paths(data: &IDataObject) -> Option<Vec<PathBuf>> {
    let format = FORMATETC {
        cfFormat: CF_HDROP.0,
        ptd: std::ptr::null_mut(),
        dwAspect: DVASPECT_CONTENT.0,
        lindex: -1,
        tymed: TYMED_HGLOBAL.0 as u32,
    };
    unsafe {
        let mut medium = data.GetData(&format).ok()?;
        let hdrop = HDROP(medium.u.hGlobal.0);
        let n = DragQueryFileW(hdrop, u32::MAX, None);
        let mut paths = Vec::with_capacity(n as usize);
        for i in 0..n {
            let len = DragQueryFileW(hdrop, i, None) as usize;
            let mut buf = vec![0u16; len + 1];
            let got = DragQueryFileW(hdrop, i, Some(&mut buf)) as usize;
            paths.push(PathBuf::from(OsString::from_wide(&buf[..got])));
        }
        ReleaseStgMedium(&mut medium);
        (!paths.is_empty()).then_some(paths)
    }
}
