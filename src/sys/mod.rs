//! Thin safe wrappers around Win32: windowing helpers (`win`), Direct2D/DirectWrite (`gfx`)
//! and libarchive (`libarchive`, for reading archives).
//! The app logic calls these instead of using `unsafe` itself.

pub mod gfx;
pub mod libarchive;
pub mod win;
