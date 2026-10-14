//! Directory change notifications: a thread per watched directory waits on
//! `FindFirstChangeNotificationW` and posts a message on every change.

use std::path::Path;
use std::sync::Arc;

use windows::Win32::Foundation::{CloseHandle, HANDLE, WAIT_OBJECT_0};
use windows::Win32::Storage::FileSystem::{
    FILE_NOTIFY_CHANGE_ATTRIBUTES, FILE_NOTIFY_CHANGE_DIR_NAME, FILE_NOTIFY_CHANGE_FILE_NAME,
    FILE_NOTIFY_CHANGE_LAST_WRITE, FILE_NOTIFY_CHANGE_SIZE, FindCloseChangeNotification,
    FindFirstChangeNotificationW, FindNextChangeNotification,
};
use windows::Win32::System::Threading::{CreateEventW, INFINITE, SetEvent, WaitForMultipleObjects};
use windows::core::PCWSTR;

use crate::files::names::to_wide_z;
use crate::sys::win::SendHwnd;

/// Event handle shared between the UI and the watcher thread; closed when both are done.
struct Event(HANDLE);

// SAFETY: kernel event handles may be used from any thread.
unsafe impl Send for Event {}
unsafe impl Sync for Event {}

impl Drop for Event {
    fn drop(&mut self) {
        unsafe {
            let _ = CloseHandle(self.0);
        }
    }
}

/// Stops watching when dropped.
pub struct Watcher {
    stop: Arc<Event>,
}

impl Drop for Watcher {
    fn drop(&mut self) {
        unsafe {
            let _ = SetEvent(self.stop.0);
        }
    }
}

impl Watcher {
    /// Posts `msg` with `wparam` to `notify` whenever something in `dir` changes.
    /// Opening the notification happens on the thread, so a slow network drive
    /// never blocks the caller.
    pub fn new(dir: &Path, notify: SendHwnd, msg: u32, wparam: usize) -> Option<Watcher> {
        let stop = Arc::new(Event(unsafe { CreateEventW(None, true, false, None) }.ok()?));
        let thread_stop = stop.clone();
        let path = to_wide_z(dir.as_os_str());
        std::thread::spawn(move || {
            let filter = FILE_NOTIFY_CHANGE_FILE_NAME
                | FILE_NOTIFY_CHANGE_DIR_NAME
                | FILE_NOTIFY_CHANGE_ATTRIBUTES
                | FILE_NOTIFY_CHANGE_SIZE
                | FILE_NOTIFY_CHANGE_LAST_WRITE;
            let Ok(change) = (unsafe { FindFirstChangeNotificationW(PCWSTR(path.as_ptr()), false, filter) }) else {
                return;
            };
            loop {
                let handles = [change, thread_stop.0];
                let r = unsafe { WaitForMultipleObjects(&handles, false, INFINITE) };
                if r != WAIT_OBJECT_0 {
                    break; // stop event, or the directory/handle went away
                }
                notify.post_with(msg, wparam);
                if unsafe { FindNextChangeNotification(change) }.is_err() {
                    break;
                }
            }
            unsafe {
                let _ = FindCloseChangeNotification(change);
            }
        });
        Some(Watcher { stop })
    }
}
