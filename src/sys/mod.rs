//! Thin safe wrappers around Win32: windowing helpers (`win`), Direct2D/DirectWrite (`gfx`),
//! libarchive (`libarchive`, for reading archives) and memory-mapped files (`mapped`, for the
//! lister).
//! The app logic calls these instead of using `unsafe` itself.

pub mod gfx;
pub mod libarchive;
pub mod mapped;
pub mod win;
