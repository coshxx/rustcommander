//! All colors in one place. A light theme later is just a second `Theme` value.

use windows::Win32::Graphics::Direct2D::Common::D2D1_COLOR_F;

pub type Color = D2D1_COLOR_F;

pub const fn rgb(hex: u32) -> Color {
    rgba(hex, 1.0)
}

pub const fn rgba(hex: u32, a: f32) -> Color {
    Color {
        r: ((hex >> 16) & 0xff) as f32 / 255.0,
        g: ((hex >> 8) & 0xff) as f32 / 255.0,
        b: (hex & 0xff) as f32 / 255.0,
        a,
    }
}

pub struct Theme {
    /// Background behind everything; also the GDI background to avoid a white flash.
    pub bg: u32,
    pub panel_bg: Color,
    pub row_alt: Color,
    pub bar_bg: Color,
    pub divider: Color,
    pub text: Color,
    pub text_dim: Color,
    pub text_hidden: Color,
    pub accent: Color,
    pub cursor_active: Color,
    pub cursor_inactive: Color,
    pub error: Color,
    pub success: Color,
    /// Marked (selected for an operation) entries.
    pub marked: Color,
    pub popup_bg: Color,
    pub popup_border: Color,
    pub edit_bg: Color,
    pub scroll_thumb: Color,
    pub scroll_thumb_hover: Color,
    pub scroll_thumb_drag: Color,
    pub scroll_track: Color,

    pub icon_folder: Color,
    pub icon_parent: Color,
    pub icon_file: Color,
    pub icon_image: Color,
    pub icon_audio: Color,
    pub icon_video: Color,
    pub icon_archive: Color,
    pub icon_code: Color,
    pub icon_exe: Color,
    pub icon_doc: Color,
}

pub const DARK: Theme = Theme {
    bg: 0x1b1d23,
    panel_bg: rgb(0x1e2027),
    row_alt: rgb(0x21242b),
    bar_bg: rgb(0x16181d),
    divider: rgb(0x2b2e36),
    text: rgb(0xd8dbe2),
    text_dim: rgb(0x80868f),
    text_hidden: rgb(0x5d626c),
    accent: rgb(0x5b93ff),
    cursor_active: rgb(0x2b4a85),
    cursor_inactive: rgb(0x2d3139),
    error: rgb(0xef6b73),
    success: rgb(0x7ec27a),
    marked: rgb(0xf2c14e),
    popup_bg: rgb(0x262932),
    popup_border: rgb(0x3a3f4b),
    edit_bg: rgb(0x0f1115),
    scroll_thumb: rgba(0xffffff, 0.16),
    scroll_thumb_hover: rgba(0xffffff, 0.32),
    scroll_thumb_drag: rgba(0xffffff, 0.46),
    scroll_track: rgba(0xffffff, 0.04),

    icon_folder: rgb(0xe8b04a),
    icon_parent: rgb(0x80868f),
    icon_file: rgb(0x9aa1ad),
    icon_image: rgb(0xc678dd),
    icon_audio: rgb(0x56b6c2),
    icon_video: rgb(0xe06c75),
    icon_archive: rgb(0xd19a66),
    icon_code: rgb(0x98c379),
    icon_exe: rgb(0x61afef),
    icon_doc: rgb(0xb5bcc9),
};

/// COLORREF (0x00BBGGRR) from 0xRRGGBB.
pub const fn colorref(hex: u32) -> u32 {
    ((hex & 0xff) << 16) | (hex & 0xff00) | ((hex >> 16) & 0xff)
}
