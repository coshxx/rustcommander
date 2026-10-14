//! Dünne Hülle um Direct2D/DirectWrite: Render-Target, ein Pinsel, Textformate.

use windows::Win32::Foundation::{HWND, RECT};
use windows::Win32::Graphics::Direct2D::Common::{D2D_RECT_F, D2D_SIZE_U};
use windows::Win32::Graphics::Direct2D::{
    D2D1_ANTIALIAS_MODE_ALIASED, D2D1_DRAW_TEXT_OPTIONS_CLIP, D2D1_FACTORY_TYPE_SINGLE_THREADED,
    D2D1_HWND_RENDER_TARGET_PROPERTIES, D2D1_PRESENT_OPTIONS_NONE, D2D1_RENDER_TARGET_PROPERTIES,
    D2D1_ROUNDED_RECT, D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE, D2D1CreateFactory, ID2D1Factory,
    ID2D1HwndRenderTarget, ID2D1SolidColorBrush,
};
use windows::Win32::Graphics::DirectWrite::{
    DWRITE_FACTORY_TYPE_SHARED, DWRITE_FONT_STRETCH_NORMAL, DWRITE_FONT_STYLE_NORMAL,
    DWRITE_FONT_WEIGHT, DWRITE_FONT_WEIGHT_NORMAL, DWRITE_FONT_WEIGHT_SEMI_BOLD,
    DWRITE_MEASURING_MODE_NATURAL, DWRITE_PARAGRAPH_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT,
    DWRITE_TEXT_ALIGNMENT_CENTER, DWRITE_TEXT_ALIGNMENT_LEADING, DWRITE_TEXT_ALIGNMENT_TRAILING,
    DWRITE_TEXT_METRICS,
    DWRITE_TRIMMING, DWRITE_TRIMMING_GRANULARITY_CHARACTER, DWRITE_WORD_WRAPPING_NO_WRAP,
    DWriteCreateFactory, IDWriteFactory, IDWriteTextFormat,
};
use windows::Win32::UI::WindowsAndMessaging::GetClientRect;
use windows::core::{BOOL, HSTRING, Result};

use crate::theme::Color;

pub type Rect = D2D_RECT_F;

pub fn rect(x: f32, y: f32, w: f32, h: f32) -> Rect {
    Rect { left: x, top: y, right: x + w, bottom: y + h }
}

pub struct Fonts {
    pub name: IDWriteTextFormat,
    pub right: IDWriteTextFormat,
    pub small: IDWriteTextFormat,
    pub small_right: IDWriteTextFormat,
    pub small_center: IDWriteTextFormat,
    pub path: IDWriteTextFormat,
    pub icon: IDWriteTextFormat,
}

pub struct Gfx {
    factory: ID2D1Factory,
    pub dwrite: IDWriteFactory,
    pub fonts: Fonts,
    target: Option<(ID2D1HwndRenderTarget, ID2D1SolidColorBrush)>,
    pub dpi: f32,
}

fn family_exists(dwrite: &IDWriteFactory, name: &str) -> bool {
    unsafe {
        let mut coll = None;
        if dwrite.GetSystemFontCollection(&mut coll, false).is_err() {
            return false;
        }
        let Some(coll) = coll else { return false };
        let (mut index, mut exists) = (0u32, BOOL(0));
        coll.FindFamilyName(&HSTRING::from(name), &mut index, &mut exists).is_ok() && exists.as_bool()
    }
}

fn format(
    dwrite: &IDWriteFactory,
    family: &str,
    size: f32,
    weight: DWRITE_FONT_WEIGHT,
    align: DWRITE_TEXT_ALIGNMENT,
) -> Result<IDWriteTextFormat> {
    unsafe {
        let f = dwrite.CreateTextFormat(
            &HSTRING::from(family),
            None,
            weight,
            DWRITE_FONT_STYLE_NORMAL,
            DWRITE_FONT_STRETCH_NORMAL,
            size,
            &HSTRING::from("de-de"),
        )?;
        f.SetTextAlignment(align)?;
        f.SetParagraphAlignment(DWRITE_PARAGRAPH_ALIGNMENT_CENTER)?;
        f.SetWordWrapping(DWRITE_WORD_WRAPPING_NO_WRAP)?;
        let sign = dwrite.CreateEllipsisTrimmingSign(&f)?;
        let trim = DWRITE_TRIMMING {
            granularity: DWRITE_TRIMMING_GRANULARITY_CHARACTER,
            delimiter: 0,
            delimiterCount: 0,
        };
        f.SetTrimming(&trim, &sign)?;
        Ok(f)
    }
}

impl Gfx {
    pub fn new() -> Result<Gfx> {
        unsafe {
            let factory: ID2D1Factory = D2D1CreateFactory(D2D1_FACTORY_TYPE_SINGLE_THREADED, None)?;
            let dwrite: IDWriteFactory = DWriteCreateFactory(DWRITE_FACTORY_TYPE_SHARED)?;

            let ui = if family_exists(&dwrite, "Segoe UI Variable Text") {
                "Segoe UI Variable Text"
            } else {
                "Segoe UI"
            };
            let icons = if family_exists(&dwrite, "Segoe Fluent Icons") {
                "Segoe Fluent Icons"
            } else {
                "Segoe MDL2 Assets"
            };
            let n = DWRITE_FONT_WEIGHT_NORMAL;
            let fonts = Fonts {
                name: format(&dwrite, ui, 13.5, n, DWRITE_TEXT_ALIGNMENT_LEADING)?,
                right: format(&dwrite, ui, 13.0, n, DWRITE_TEXT_ALIGNMENT_TRAILING)?,
                small: format(&dwrite, ui, 12.0, n, DWRITE_TEXT_ALIGNMENT_LEADING)?,
                small_right: format(&dwrite, ui, 12.0, n, DWRITE_TEXT_ALIGNMENT_TRAILING)?,
                small_center: format(&dwrite, ui, 11.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_CENTER)?,
                path: format(&dwrite, ui, 13.5, DWRITE_FONT_WEIGHT_SEMI_BOLD, DWRITE_TEXT_ALIGNMENT_LEADING)?,
                icon: format(&dwrite, icons, 15.0, n, DWRITE_TEXT_ALIGNMENT_CENTER)?,
            };
            Ok(Gfx { factory, dwrite, fonts, target: None, dpi: 96.0 })
        }
    }

    fn client_size(hwnd: HWND) -> D2D_SIZE_U {
        let mut rc = RECT::default();
        unsafe {
            let _ = GetClientRect(hwnd, &mut rc);
        }
        D2D_SIZE_U { width: (rc.right - rc.left).max(1) as u32, height: (rc.bottom - rc.top).max(1) as u32 }
    }

    pub fn ensure_target(&mut self, hwnd: HWND) -> Result<()> {
        if let Some((t, _)) = &self.target {
            // Selbstheilend, falls ein WM_SIZE verloren ging.
            let want = Self::client_size(hwnd);
            let have = unsafe { t.GetPixelSize() };
            if have.width != want.width || have.height != want.height {
                self.resize(hwnd);
            }
            if self.target.is_some() {
                return Ok(());
            }
        }
        unsafe {
            let mut props = D2D1_RENDER_TARGET_PROPERTIES { dpiX: self.dpi, dpiY: self.dpi, ..Default::default() };
            // Software-Rendering: das Hardware-Target kostet beim Start ~150 ms (D3D/Treiber laden),
            // zeichnet aber nicht schneller. Gemessen: Start 40 statt 175 ms, Frame 7–12 ms.
            if std::env::var_os("RC_HARDWARE").is_none() {
                props.r#type = windows::Win32::Graphics::Direct2D::D2D1_RENDER_TARGET_TYPE_SOFTWARE;
            }
            let hprops = D2D1_HWND_RENDER_TARGET_PROPERTIES {
                hwnd,
                pixelSize: Self::client_size(hwnd),
                presentOptions: D2D1_PRESENT_OPTIONS_NONE,
            };
            let target = self.factory.CreateHwndRenderTarget(&props, &hprops)?;
            target.SetTextAntialiasMode(D2D1_TEXT_ANTIALIAS_MODE_CLEARTYPE);
            let brush = target.CreateSolidColorBrush(&Color { r: 1.0, g: 1.0, b: 1.0, a: 1.0 }, None)?;
            self.target = Some((target, brush));
        }
        Ok(())
    }

    pub fn resize(&mut self, hwnd: HWND) {
        if let Some((t, _)) = &self.target {
            unsafe {
                if t.Resize(&Self::client_size(hwnd)).is_err() {
                    self.target = None;
                }
            }
        }
    }

    pub fn set_dpi(&mut self, dpi: f32) {
        self.dpi = dpi;
        if let Some((t, _)) = &self.target {
            unsafe { t.SetDpi(dpi, dpi) };
        }
    }

    /// Größe des Client-Bereichs in DIPs.
    pub fn size_dips(&self, hwnd: HWND) -> (f32, f32) {
        let s = Self::client_size(hwnd);
        let k = 96.0 / self.dpi;
        (s.width as f32 * k, s.height as f32 * k)
    }

    /// Textbreite in DIPs (inkl. Leerzeichen am Ende).
    pub fn measure(&self, s: &[u16], f: &IDWriteTextFormat) -> f32 {
        unsafe {
            let Ok(layout) = self.dwrite.CreateTextLayout(s, f, 10000.0, 100.0) else {
                return 0.0;
            };
            let mut m = DWRITE_TEXT_METRICS::default();
            if layout.GetMetrics(&mut m).is_ok() { m.widthIncludingTrailingWhitespace } else { 0.0 }
        }
    }

    pub fn caret_x(&self, before_caret: &[u16]) -> f32 {
        self.measure(before_caret, &self.fonts.name)
    }

    pub fn begin(&self, clear: Color) -> Option<Canvas<'_>> {
        let (target, brush) = self.target.as_ref()?;
        unsafe {
            target.BeginDraw();
            target.Clear(Some(&clear));
        }
        Some(Canvas { target, brush, fonts: &self.fonts })
    }

    /// Frame abschließen; bei verlorenem Gerät wird das Target neu erzeugt.
    pub fn end(&mut self) {
        if let Some((t, _)) = &self.target {
            if unsafe { t.EndDraw(None, None) }.is_err() {
                self.target = None;
            }
        }
    }
}

pub struct Canvas<'a> {
    target: &'a ID2D1HwndRenderTarget,
    brush: &'a ID2D1SolidColorBrush,
    pub fonts: &'a Fonts,
}

impl Canvas<'_> {
    fn color(&self, c: Color) {
        unsafe { self.brush.SetColor(&c) };
    }

    pub fn fill(&self, r: Rect, c: Color) {
        self.color(c);
        unsafe { self.target.FillRectangle(&r, self.brush) };
    }

    pub fn fill_round(&self, r: Rect, radius: f32, c: Color) {
        self.color(c);
        let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
        unsafe { self.target.FillRoundedRectangle(&rr, self.brush) };
    }

    pub fn stroke_round(&self, r: Rect, radius: f32, width: f32, c: Color) {
        self.color(c);
        let rr = D2D1_ROUNDED_RECT { rect: r, radiusX: radius, radiusY: radius };
        unsafe { self.target.DrawRoundedRectangle(&rr, self.brush, width, None) };
    }

    pub fn text(&self, s: &[u16], f: &IDWriteTextFormat, r: Rect, c: Color) {
        if s.is_empty() || r.right <= r.left {
            return;
        }
        self.color(c);
        unsafe {
            self.target.DrawText(
                s,
                f,
                &r,
                self.brush,
                D2D1_DRAW_TEXT_OPTIONS_CLIP,
                DWRITE_MEASURING_MODE_NATURAL,
            )
        };
    }

    pub fn text_str(&self, s: &str, f: &IDWriteTextFormat, r: Rect, c: Color) {
        let w: Vec<u16> = s.encode_utf16().collect();
        self.text(&w, f, r, c);
    }

    pub fn clip(&self, r: Rect) {
        unsafe { self.target.PushAxisAlignedClip(&r, D2D1_ANTIALIAS_MODE_ALIASED) };
    }

    pub fn unclip(&self) {
        unsafe { self.target.PopAxisAlignedClip() };
    }
}
