//! The window's visual rules in one place: a type scale, a spacing scale and colour
//! roles, plus the GDI helpers that draw with them.

use std::ptr::null_mut;

use windows_sys::Win32::Foundation::{HWND, RECT, SIZE};
use windows_sys::Win32::Graphics::Gdi::{
    CLEARTYPE_QUALITY, CLIP_DEFAULT_PRECIS, CreateFontW, CreatePen, CreateSolidBrush,
    DEFAULT_CHARSET, DRAW_TEXT_FORMAT, DT_END_ELLIPSIS, DT_LEFT, DT_NOPREFIX, DT_SINGLELINE,
    DT_VCENTER, DeleteObject, DrawTextW, FW_NORMAL, FW_SEMIBOLD, GetDC, GetTextExtentExPointW,
    GetTextExtentPoint32W, GetTextFaceW, GetTextMetricsW, HDC, HFONT, OUT_DEFAULT_PRECIS, PS_SOLID,
    ReleaseDC, RoundRect, SelectObject, SetBkMode, SetTextColor, TEXTMETRICW, TRANSPARENT,
};
use windows_sys::Win32::UI::HiDpi::GetDpiForWindow;

/// Spacing scale, in pixels at 96 DPI. Every gap in the window is one of these.
pub const SPACE_S: i32 = 4;
pub const SPACE_M: i32 = 8;
pub const SPACE_L: i32 = 12;
pub const SPACE_XL: i32 = 16;
/// Height of a result row and of a section heading.
pub const ROW_HEIGHT: i32 = 48;
/// Corner radius of fields, rows and buttons.
pub const RADIUS: i32 = 6;

pub const GLYPH_SEARCH: &str = "\u{E721}";
pub const GLYPH_FOLDER: &str = "\u{E838}";
pub const GLYPH_COPY: &str = "\u{E8C8}";

pub fn scale(hwnd: HWND, value: i32) -> i32 {
    let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96);
    value * dpi as i32 / 96
}

/// Colour roles. Everything drawn uses one of these, so light and dark stay consistent.
#[derive(Clone, Copy)]
pub struct Palette {
    /// The window background.
    pub panel: u32,
    /// Raised surfaces: the search field.
    pub surface: u32,
    pub hover: u32,
    pub selected: u32,
    pub text: u32,
    /// Second lines, headings, the footer.
    pub secondary: u32,
    pub edge: u32,
    pub accent: u32,
}

pub fn rgb(r: u32, g: u32, b: u32) -> u32 {
    r | (g << 8) | (b << 16)
}

/// Mixes `color` toward `other` by `amount` out of 100.
fn mix(color: u32, other: u32, amount: u32) -> u32 {
    let channel = |shift: u32| {
        let a = (color >> shift) & 0xff;
        let b = (other >> shift) & 0xff;
        ((a * (100 - amount) + b * amount) / 100) << shift
    };
    channel(0) | channel(8) | channel(16)
}

/// Windows 11's own base colours for each theme.
pub fn palette(light: bool, accent: u32) -> Palette {
    if light {
        Palette {
            panel: rgb(243, 243, 243),
            surface: rgb(255, 255, 255),
            hover: rgb(233, 233, 233),
            selected: rgb(224, 224, 224),
            text: rgb(26, 26, 26),
            secondary: rgb(96, 96, 96),
            edge: rgb(222, 222, 222),
            accent,
        }
    } else {
        Palette {
            panel: rgb(32, 32, 32),
            surface: rgb(45, 45, 45),
            hover: rgb(45, 45, 45),
            selected: rgb(56, 56, 56),
            text: rgb(255, 255, 255),
            secondary: rgb(170, 170, 170),
            edge: rgb(58, 58, 58),
            // Windows uses a lighter accent on dark backgrounds.
            accent: mix(accent, rgb(255, 255, 255), 35),
        }
    }
}

/// Type scale: one face, a few sizes and two weights.
pub struct Fonts {
    /// Settings and other plain controls, 10 pt.
    pub ui: HFONT,
    /// The search box, 12 pt.
    pub search: HFONT,
    /// Result names, 10.5 pt, with matched letters in semibold.
    pub name: HFONT,
    pub name_bold: HFONT,
    /// Second lines and the footer, 9 pt.
    pub detail: HFONT,
    /// Section headings, 9 pt semibold.
    pub heading: HFONT,
    /// Segoe Fluent Icons (Windows 11) or Segoe MDL2 Assets (Windows 10), 12 pt.
    pub glyph: HFONT,
}

impl Fonts {
    pub fn new(hwnd: HWND) -> Self {
        let dpi = unsafe { GetDpiForWindow(hwnd) }.max(96) as i32;
        // Tenths of a point to a negative character height in pixels.
        let size = |tenths: i32| -(tenths * dpi / 720);
        let face = if has_face(hwnd, "Segoe UI Variable Text") {
            "Segoe UI Variable Text"
        } else {
            "Segoe UI"
        };
        let glyphs = if has_face(hwnd, "Segoe Fluent Icons") {
            "Segoe Fluent Icons"
        } else {
            "Segoe MDL2 Assets"
        };
        Self {
            ui: font(size(100), FW_NORMAL, face),
            search: font(size(120), FW_NORMAL, face),
            name: font(size(105), FW_NORMAL, face),
            name_bold: font(size(105), FW_SEMIBOLD, face),
            detail: font(size(90), FW_NORMAL, face),
            heading: font(size(90), FW_SEMIBOLD, face),
            glyph: font(size(120), FW_NORMAL, glyphs),
        }
    }

    pub fn delete(&self) {
        for font in [
            self.ui,
            self.search,
            self.name,
            self.name_bold,
            self.detail,
            self.heading,
            self.glyph,
        ] {
            unsafe { DeleteObject(font) };
        }
    }
}

fn font(height: i32, weight: u32, face: &str) -> HFONT {
    let face: Vec<u16> = face.encode_utf16().chain([0]).collect();
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight as i32,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            0,
            face.as_ptr(),
        )
    }
}

/// Whether Windows has `face` rather than substituting another font for it.
fn has_face(hwnd: HWND, face: &str) -> bool {
    let probe = font(-12, FW_NORMAL, face);
    let device = unsafe { GetDC(hwnd) };
    if device.is_null() {
        unsafe { DeleteObject(probe) };
        return false;
    }
    let previous = unsafe { SelectObject(device, probe) };
    let mut buffer = [0u16; 64];
    let length = unsafe { GetTextFaceW(device, buffer.len() as i32, buffer.as_mut_ptr()) };
    unsafe {
        SelectObject(device, previous);
        ReleaseDC(hwnd, device);
        DeleteObject(probe);
    }
    let end = (length.max(1) as usize - 1).min(buffer.len());
    String::from_utf16_lossy(&buffer[..end]).eq_ignore_ascii_case(face)
}

/// A filled rounded rectangle without an outline.
pub fn fill_round(dc: HDC, rect: &RECT, radius: i32, color: u32) {
    unsafe {
        let brush = CreateSolidBrush(color);
        let pen = CreatePen(PS_SOLID, 1, color);
        let old_brush = SelectObject(dc, brush);
        let old_pen = SelectObject(dc, pen);
        RoundRect(
            dc,
            rect.left,
            rect.top,
            rect.right,
            rect.bottom,
            radius * 2,
            radius * 2,
        );
        SelectObject(dc, old_brush);
        SelectObject(dc, old_pen);
        DeleteObject(brush);
        DeleteObject(pen);
    }
}

/// Single-line text in `rect`, vertically centred. `extra` adds flags such as
/// `DT_PATH_ELLIPSIS` or `DT_RIGHT`.
pub fn text(dc: HDC, value: &str, rect: &RECT, font: HFONT, color: u32, extra: DRAW_TEXT_FORMAT) {
    // DrawTextW reads the first character even for a count of 0, and an empty Vec's
    // pointer is dangling, so empty text (a name run cut to nothing) crashed it.
    if value.is_empty() {
        return;
    }
    let value: Vec<u16> = value.encode_utf16().chain([0]).collect();
    let value = &value[..value.len() - 1];
    let mut rect = *rect;
    unsafe {
        let old = SelectObject(dc, font);
        SetBkMode(dc, TRANSPARENT as i32);
        SetTextColor(dc, color);
        DrawTextW(
            dc,
            value.as_ptr(),
            value.len() as i32,
            &mut rect,
            DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX | DT_LEFT | extra,
        );
        SelectObject(dc, old);
    }
}

pub fn line_height(dc: HDC, font: HFONT) -> i32 {
    let mut metrics = TEXTMETRICW::default();
    unsafe {
        let old = SelectObject(dc, font);
        GetTextMetricsW(dc, &mut metrics);
        SelectObject(dc, old);
    }
    metrics.tmHeight
}

pub fn text_width(dc: HDC, font: HFONT, value: &[u16]) -> i32 {
    if value.is_empty() {
        return 0;
    }
    let mut size = SIZE::default();
    unsafe {
        let old = SelectObject(dc, font);
        GetTextExtentPoint32W(dc, value.as_ptr(), value.len() as i32, &mut size);
        SelectObject(dc, old);
    }
    size.cx
}

/// A name with its matched letters in semibold, cut with an ellipsis if it does not
/// fit `rect`.
pub fn marked_text(
    dc: HDC,
    name: &[u16],
    marks: &[bool],
    rect: &RECT,
    fonts: (HFONT, HFONT),
    color: u32,
) {
    let (normal, bold) = fonts;
    if marks.len() != name.len() || !marks.contains(&true) {
        let value = String::from_utf16_lossy(name);
        text(dc, &value, rect, normal, color, DT_END_ELLIPSIS);
        return;
    }
    let ellipsis: Vec<u16> = "…".encode_utf16().collect();
    let room = rect.right - rect.left;
    let total: i32 = runs(marks)
        .map(|(from, to, bold_run)| {
            text_width(dc, if bold_run { bold } else { normal }, &name[from..to])
        })
        .sum();
    let budget = if total > room {
        room - text_width(dc, normal, &ellipsis)
    } else {
        room
    };
    let mut x = rect.left;
    for (from, to, bold_run) in runs(marks) {
        let font = if bold_run { bold } else { normal };
        let run = &name[from..to];
        let left = budget - (x - rect.left);
        let mut fit = run.len() as i32;
        if text_width(dc, font, run) > left {
            unsafe {
                let old = SelectObject(dc, font);
                let mut size = SIZE::default();
                GetTextExtentExPointW(
                    dc,
                    run.as_ptr(),
                    run.len() as i32,
                    left.max(0),
                    &mut fit,
                    null_mut(),
                    &mut size,
                );
                SelectObject(dc, old);
            }
        }
        let shown = &run[..fit.clamp(0, run.len() as i32) as usize];
        let piece = RECT {
            left: x,
            right: rect.right,
            ..*rect
        };
        text(dc, &String::from_utf16_lossy(shown), &piece, font, color, 0);
        x += text_width(dc, font, shown);
        if (fit as usize) < run.len() {
            let piece = RECT {
                left: x,
                right: rect.right,
                ..*rect
            };
            text(dc, "…", &piece, normal, color, 0);
            return;
        }
    }
}

/// `(from, to, marked)` runs of equal marks.
fn runs(marks: &[bool]) -> impl Iterator<Item = (usize, usize, bool)> + '_ {
    let mut start = 0;
    (1..=marks.len()).filter_map(move |i| {
        if i == marks.len() || marks[i] != marks[start] {
            let run = (start, i, marks[start]);
            start = i;
            Some(run)
        } else {
            None
        }
    })
}

pub const fn centered(outer: &RECT, height: i32) -> RECT {
    let top = outer.top + (outer.bottom - outer.top - height) / 2;
    RECT {
        left: outer.left,
        top,
        right: outer.right,
        bottom: top + height,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn runs_split_marks_and_colours_mix() {
        let marks = [true, true, false, true];
        let found: Vec<_> = runs(&marks).collect();
        assert_eq!(found, [(0, 2, true), (2, 3, false), (3, 4, true)]);
        assert_eq!(runs(&[]).count(), 0);
        assert_eq!(mix(rgb(0, 0, 0), rgb(200, 100, 0), 50), rgb(100, 50, 0));
    }

    #[test]
    fn drawing_empty_text_is_safe() {
        use windows_sys::Win32::Graphics::Gdi::{CreateCompatibleDC, DeleteDC};
        let dc = unsafe { CreateCompatibleDC(null_mut()) };
        let rect = RECT {
            left: 0,
            top: 0,
            right: 40,
            bottom: 20,
        };
        for flags in [0, DT_END_ELLIPSIS] {
            text(dc, "", &rect, null_mut(), 0, flags);
            text(dc, "abc", &rect, null_mut(), 0, flags);
        }
        let name: Vec<u16> = "abcdefgh".encode_utf16().collect();
        let marks = [true, true, false, false, true, true, true, true];
        // Too narrow for any run, so every piece is cut to nothing.
        let narrow = RECT { right: 1, ..rect };
        marked_text(dc, &name, &marks, &narrow, (null_mut(), null_mut()), 0);
        unsafe { DeleteDC(dc) };
    }
}
