//! Thin safe wrappers around Win32: windowing helpers (`win`), Direct2D/DirectWrite (`gfx`),
//! libarchive (`libarchive`, for reading archives), memory-mapped files (`mapped`, for the
//! lister), the shell context menu and dragging out (`shell_menu`) and dropping onto the window
//! (`drop_target`).
//! The app logic calls these instead of using `unsafe` itself.

pub mod drop_target;
pub mod gfx;
pub mod libarchive;
pub mod mapped;
pub mod shell_menu;
pub mod win;
