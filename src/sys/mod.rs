//! Thin safe wrappers around Win32: windowing helpers (`win`) and Direct2D/DirectWrite (`gfx`).
//! The app logic calls these instead of using `unsafe` itself.

pub mod gfx;
pub mod win;
