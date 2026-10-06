//! The settings page, drawn by the parent like the rest of the panel: section headings,
//! rounded rows with a title and a second line, Windows 11-style switches, and the one
//! native control left on the page, the hotkey field. `main.rs` builds the page when
//! its inputs change, paints it from WM_PAINT and asks it where a click landed.

use windows_sys::Win32::Foundation::RECT;

use crate::draw;

/// Which switch a row flips, so the click can apply the right setting.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Toggle {
    StartWithWindows,
    History,
}

/// Which action a row runs.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Action {
    WindowsSearch,
    Updates,
    ClearHistory,
}

/// One line of the page, top to bottom.
#[derive(Clone, Debug)]
pub enum Row {
    Heading(&'static str),
    Toggle {
        kind: Toggle,
        title: &'static str,
        detail: &'static str,
        on: bool,
    },
    Action {
        kind: Action,
        title: String,
        detail: String,
    },
    /// Plain text, not interactive: the service's facts and the footprint.
    Note {
        text: String,
    },
}

pub struct Line {
    pub row: Row,
    /// The whole area a click or hover refers to, in page coordinates.
    pub rect: RECT,
}

pub struct Page {
    pub lines: Vec<Line>,
    /// The hotkey control's band; the control itself is placed by `main.rs`.
    pub hotkey: RECT,
    /// The back button at the top left.
    pub back: RECT,
    /// The page title next to it.
    pub title: RECT,
    /// Full content height, for wheel scrolling.
    pub content: i32,
}

impl Page {
    /// The line under a point, ignoring scroll (callers add the scroll offset).
    pub fn line_at(&self, x: i32, y: i32) -> Option<usize> {
        self.lines.iter().position(|line| {
            line.rect.left <= x && x < line.rect.right && line.rect.top <= y && y < line.rect.bottom
        })
    }

    pub fn is_heading(&self, index: usize) -> bool {
        matches!(self.lines.get(index).map(|l| &l.row), Some(Row::Heading(_)))
    }
}

/// Everything the page's wording depends on. The caller measures wrapped notes,
/// so this stays free of device contexts.
pub struct Inputs {
    pub start_with_windows: bool,
    pub history: bool,
    /// What the Windows search row says now, and the state under it.
    pub windows_search: (String, String),
    /// The footprint note under the Windows search section.
    pub sizes: String,
    /// The fixed NTFS drives the service indexes.
    pub drives: String,
}

const ROW: i32 = draw::ROW_HEIGHT;
/// One line of a heading, plus the room around it.
const HEADING: i32 = 28;
/// The hotkey band matches the search field's height.
const HOTKEY_BAND: i32 = 40;
/// The back button is a square this size, like the search field's gear.
const BACK: i32 = 36;

/// `RegisterHotKey`'s modifier flags, in the order the combo is written.
const MOD_CONTROL: u32 = 0x2;
const MOD_SHIFT: u32 = 0x4;
const MOD_ALT: u32 = 0x1;
const MOD_WIN: u32 = 0x8;

/// Whether a captured combination can be registered: a modifier with any key, or
/// one of F1-F24 alone.
pub fn valid_combo(modifiers: u32, key: u32) -> bool {
    modifiers != 0 || (0x70..=0x87).contains(&key)
}

/// The keys as Windows writes them: `Ctrl + Shift + Space`.
pub fn hotkey_name(modifiers: u32, key: u32) -> String {
    let mut parts: Vec<String> = Vec::new();
    if modifiers & MOD_CONTROL != 0 {
        parts.push("Ctrl".into());
    }
    if modifiers & MOD_SHIFT != 0 {
        parts.push("Shift".into());
    }
    if modifiers & MOD_ALT != 0 {
        parts.push("Alt".into());
    }
    if modifiers & MOD_WIN != 0 {
        parts.push("Win".into());
    }
    parts.push(match key {
        0 => return parts.join(" + "),
        0x09 => "Tab".into(),
        0x0d => "Enter".into(),
        0x14 => "Caps Lock".into(),
        0x20 => "Space".into(),
        0x21 => "Page Up".into(),
        0x22 => "Page Down".into(),
        0x23 => "End".into(),
        0x24 => "Home".into(),
        0x25 => "Left".into(),
        0x26 => "Up".into(),
        0x27 => "Right".into(),
        0x28 => "Down".into(),
        0x2d => "Insert".into(),
        0x2e => "Delete".into(),
        0x30..=0x39 | 0x41..=0x5a => (key as u8 as char).to_string(),
        0x70..=0x87 => format!("F{}", key - 0x70 + 1),
        _ => format!("Key {key}"),
    });
    parts.join(" + ")
}

/// Lays the page out top to bottom. `s` scales a 96-DPI pixel; `note_height` gives
/// the wrapped height of a note's text.
pub fn build(
    width: i32,
    s: impl Fn(i32) -> i32,
    note_height: impl Fn(&str) -> i32,
    inputs: &Inputs,
) -> Page {
    let left = s(draw::SPACE_XL);
    let right = left + width;
    let mut y = s(draw::SPACE_XL);
    let mut lines = Vec::new();
    let back_square = s(BACK);
    let back = RECT {
        left,
        top: y,
        right: left + back_square,
        bottom: y + back_square,
    };
    let title = RECT {
        left: back.right + s(draw::SPACE_M),
        right,
        ..back
    };
    y += back_square + s(draw::SPACE_M);

    // One row at `y`, then the gap below it. Every line goes through this.
    let mut push = |row: Row, y: &mut i32, height: i32| {
        let rect = RECT {
            left,
            top: *y,
            right,
            bottom: *y + height,
        };
        lines.push(Line { row, rect });
        *y += height + s(draw::SPACE_M);
    };

    // OPEN WITH
    push(Row::Heading("OPEN WITH"), &mut y, s(HEADING));
    let hotkey = RECT {
        left,
        top: y,
        right,
        bottom: y + s(HOTKEY_BAND),
    };
    y = hotkey.bottom + s(draw::SPACE_M);

    // BEHAVIOR
    push(Row::Heading("BEHAVIOR"), &mut y, s(HEADING));
    push(
        Row::Toggle {
            kind: Toggle::StartWithWindows,
            title: "Start with Windows",
            detail: "Starts hidden in the tray (current user only)",
            on: inputs.start_with_windows,
        },
        &mut y,
        s(ROW),
    );
    push(
        Row::Toggle {
            kind: Toggle::History,
            title: "Rank what I open higher",
            detail: "Keeps an open history on this PC",
            on: inputs.history,
        },
        &mut y,
        s(ROW),
    );
    push(
        Row::Action {
            kind: Action::ClearHistory,
            title: "Clear open history".into(),
            detail: "Forgets what was opened from the panel".into(),
        },
        &mut y,
        s(ROW),
    );

    // WINDOWS SEARCH
    push(Row::Heading("WINDOWS SEARCH"), &mut y, s(HEADING));
    let (switch_title, switch_detail) = inputs.windows_search.clone();
    push(
        Row::Action {
            kind: Action::WindowsSearch,
            title: switch_title,
            detail: switch_detail,
        },
        &mut y,
        s(ROW),
    );
    let height = note_height(&inputs.sizes);
    push(
        Row::Note {
            text: inputs.sizes.clone(),
        },
        &mut y,
        height + 2 * s(draw::SPACE_M),
    );

    // SERVICE
    push(Row::Heading("SERVICE"), &mut y, s(HEADING));
    let drives = format!(
        "The service indexes every fixed NTFS drive: {}. Choosing drives is not built yet.",
        inputs.drives
    );
    let height = note_height(&drives);
    push(
        Row::Note { text: drives },
        &mut y,
        height + 2 * s(draw::SPACE_M),
    );
    let deferred = "Skipped folders' contents are not indexed, so counts of skipped files \
                    and per-folder overrides cannot be honest yet.";
    let height = note_height(deferred);
    push(
        Row::Note {
            text: deferred.into(),
        },
        &mut y,
        height + 2 * s(draw::SPACE_M),
    );

    // ABOUT
    push(Row::Heading("ABOUT"), &mut y, s(HEADING));
    push(
        Row::Action {
            kind: Action::Updates,
            title: format!("Check for updates (installed {})", crate::update::current()),
            detail: "Downloads from GitHub and asks before installing".into(),
        },
        &mut y,
        s(ROW),
    );

    Page {
        lines,
        hotkey,
        back,
        title,
        content: y,
    }
}

/// What the page looks like right now: the mouse's row, the keyboard's row, the
/// hotkey band's text and capture state, and how far the page is scrolled.
#[derive(Clone, Default)]
pub struct View {
    pub hover: Option<usize>,
    pub cursor: Option<usize>,
    /// What the hotkey band shows on its right: the current combination, or, while
    /// the band is being edited, the keys held so far.
    pub hotkey_text: String,
    pub capturing: bool,
    pub offset: i32,
}

/// Paints the page shifted up by the view's scroll offset.
pub fn paint(
    device: windows_sys::Win32::Graphics::Gdi::HDC,
    page: &Page,
    fonts: &draw::Fonts,
    colors: &draw::Palette,
    view: &View,
    s: &dyn Fn(i32) -> i32,
) {
    use windows_sys::Win32::Graphics::Gdi::{DT_CENTER, DT_END_ELLIPSIS};

    let View {
        hover,
        cursor,
        hotkey_text,
        capturing,
        offset,
    } = view;
    let (hover, cursor) = (*hover, *cursor);
    let hotkey_text = hotkey_text.clone();
    let capturing = *capturing;
    let at = |rect: &RECT| RECT {
        top: rect.top - offset,
        bottom: rect.bottom - offset,
        ..*rect
    };
    let visible = |rect: &RECT| rect.bottom >= 0;

    let back = at(&page.back);
    if visible(&back) {
        draw::text(
            device,
            draw::GLYPH_BACK,
            &back,
            fonts.glyph,
            colors.secondary,
            DT_CENTER,
        );
    }
    let title = at(&page.title);
    if visible(&title) {
        draw::text(device, "Settings", &title, fonts.name, colors.text, 0);
    }

    for (index, line) in page.lines.iter().enumerate() {
        let rect = at(&line.rect);
        if !visible(&rect) {
            continue;
        }
        match &line.row {
            Row::Heading(text) => {
                draw::text(device, text, &rect, fonts.heading, colors.secondary, 0);
            }
            Row::Toggle {
                title, detail, on, ..
            } => {
                let card = card_rect(&rect, s);
                draw::fill_round(device, &card, s(draw::RADIUS), colors.surface);
                if hover == Some(index) {
                    draw::fill_round(device, &card, s(draw::RADIUS), colors.hover);
                }
                if cursor == Some(index) {
                    draw::fill_round(device, &card, s(draw::RADIUS), colors.selected);
                }
                row_text(device, &card, title, detail, fonts, colors);
                switch(device, &card, *on, colors, s);
            }
            Row::Action { title, detail, .. } => {
                let card = card_rect(&rect, s);
                draw::fill_round(device, &card, s(draw::RADIUS), colors.surface);
                if hover == Some(index) || cursor == Some(index) {
                    draw::fill_round(device, &card, s(draw::RADIUS), colors.hover);
                }
                row_text(device, &card, title, detail, fonts, colors);
                let square = card.bottom - card.top;
                let chevron = RECT {
                    left: card.right - square,
                    right: card.right,
                    ..card
                };
                draw::text(
                    device,
                    draw::GLYPH_CHEVRON,
                    &chevron,
                    fonts.glyph,
                    colors.secondary,
                    DT_CENTER,
                );
            }
            Row::Note { text, .. } => {
                let card = card_rect(&rect, s);
                let text_rect = RECT {
                    left: card.left + s(draw::SPACE_L),
                    top: rect.top + s(draw::SPACE_M),
                    right: card.right - s(draw::SPACE_L),
                    bottom: rect.bottom - s(draw::SPACE_M),
                };
                draw::wrapped_text(device, text, &text_rect, fonts.detail, colors.secondary);
            }
        }
    }

    // The hotkey band, drawn like the search field: edge, then surface, then the
    // accent underline while the combination is being captured. The band's right
    // side shows the current combination, or the capture's prompt.
    let band = at(&page.hotkey);
    if visible(&band) {
        let radius = s(draw::RADIUS);
        draw::fill_round(device, &band, radius, colors.edge);
        let inner = RECT {
            left: band.left + 1,
            top: band.top + 1,
            right: band.right - 1,
            bottom: band.bottom - 1,
        };
        draw::fill_round(device, &inner, radius, colors.surface);
        if capturing {
            let line = RECT {
                left: band.left + radius / 2,
                top: band.bottom - s(2),
                right: band.right - radius / 2,
                bottom: band.bottom,
            };
            let brush =
                unsafe { windows_sys::Win32::Graphics::Gdi::CreateSolidBrush(colors.accent) };
            unsafe {
                windows_sys::Win32::Graphics::Gdi::FillRect(device, &line, brush);
                windows_sys::Win32::Graphics::Gdi::DeleteObject(brush);
            }
        }
        let label = RECT {
            left: band.left + s(draw::SPACE_L),
            right: band.left + s(draw::SPACE_L) + s(120),
            ..band
        };
        draw::text(device, "Hotkey", &label, fonts.name, colors.text, 0);
        let value_rect = RECT {
            left: label.right + s(draw::SPACE_M),
            right: band.right - s(draw::SPACE_L),
            ..band
        };
        if !hotkey_text.is_empty() {
            let color = if capturing {
                colors.secondary
            } else {
                colors.text
            };
            draw::text(
                device,
                &hotkey_text,
                &value_rect,
                fonts.name,
                color,
                DT_END_ELLIPSIS,
            );
        } else if !capturing {
            draw::text(
                device,
                "Not set",
                &value_rect,
                fonts.name,
                colors.secondary,
                0,
            );
        }
    }
}

/// Rows highlight inside their card, so the fill lines up with the page's edges.
fn card_rect(rect: &RECT, s: &dyn Fn(i32) -> i32) -> RECT {
    RECT {
        left: rect.left - s(draw::SPACE_S),
        right: rect.right + s(draw::SPACE_S),
        ..*rect
    }
}

fn row_text(
    device: windows_sys::Win32::Graphics::Gdi::HDC,
    card: &RECT,
    title: &str,
    detail: &str,
    fonts: &draw::Fonts,
    colors: &draw::Palette,
) {
    use windows_sys::Win32::Graphics::Gdi::DT_END_ELLIPSIS;

    let s = card.bottom - card.top;
    let left = card.left + s / 2;
    let right = card.right - s / 2;
    let title_rect = RECT {
        left,
        right,
        bottom: card.top + s / 2,
        ..*card
    };
    draw::text(
        device,
        title,
        &title_rect,
        fonts.name,
        colors.text,
        DT_END_ELLIPSIS,
    );
    let detail_rect = RECT {
        left,
        right,
        top: card.top + s / 2,
        ..*card
    };
    draw::text(
        device,
        detail,
        &detail_rect,
        fonts.detail,
        colors.secondary,
        DT_END_ELLIPSIS,
    );
}

/// A Windows 11 switch: a pill on the row's right, accent when on.
fn switch(
    device: windows_sys::Win32::Graphics::Gdi::HDC,
    card: &RECT,
    on: bool,
    colors: &draw::Palette,
    s: &dyn Fn(i32) -> i32,
) {
    let height = s(20);
    let width = s(40);
    let middle = card.top + (card.bottom - card.top) / 2;
    let pill = RECT {
        left: card.right - s(draw::SPACE_L) - width,
        top: middle - height / 2,
        right: card.right - s(draw::SPACE_L),
        bottom: middle + height / 2,
    };
    let fill = if on { colors.accent } else { colors.selected };
    draw::fill_round(device, &pill, height / 2, fill);
    // The knob: a circle drawn as a square whose radius is half its side.
    let knob = s(14);
    let x = if on {
        pill.right - knob - s(3)
    } else {
        pill.left + s(3)
    };
    let knob_rect = RECT {
        left: x,
        top: middle - knob / 2,
        right: x + knob,
        bottom: middle + knob / 2,
    };
    draw::fill_round(device, &knob_rect, knob / 2, draw::rgb(255, 255, 255));
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> Inputs {
        Inputs {
            start_with_windows: false,
            history: true,
            windows_search: (
                "Turn Windows search off (asks for admin)".into(),
                "Windows search is on".into(),
            ),
            sizes: "Using 25 MB of memory and keeping a 4 MB index".into(),
            drives: "C:, D:, E:".into(),
        }
    }

    fn page(width: i32) -> Page {
        build(width, |v| v, |_| 30, &inputs())
    }

    #[test]
    fn lines_stack_without_gaps_or_overlap() {
        let page = page(400);
        for pair in page.lines.windows(2) {
            let (a, b) = (&pair[0].rect, &pair[1].rect);
            assert!(
                a.bottom <= b.top,
                "{}..{} then {}..{}",
                a.top,
                a.bottom,
                b.top,
                b.bottom
            );
        }
        assert!(page.lines[0].rect.left >= 0);
    }

    #[test]
    fn every_row_is_reachable_and_typed() {
        let page = page(400);
        assert!(matches!(page.lines[0].row, Row::Heading("OPEN WITH")));
        // The next interactive line comes after the hotkey band.
        let hotkey_at = page
            .lines
            .iter()
            .position(|l| l.rect.top >= page.hotkey.bottom);
        assert!(hotkey_at.is_some());
        for (index, line) in page.lines.iter().enumerate() {
            if matches!(line.row, Row::Heading(_)) {
                assert!(page.is_heading(index));
                continue;
            }
            let middle = (
                line.rect.left + (line.rect.right - line.rect.left) / 2,
                line.rect.top + (line.rect.bottom - line.rect.top) / 2,
            );
            assert_eq!(
                page.line_at(middle.0, middle.1),
                Some(index),
                "line {index} at {}..{}",
                line.rect.top,
                line.rect.bottom
            );
        }
    }

    #[test]
    fn the_switch_holds_its_state_in_the_row() {
        let mut inputs = inputs();
        inputs.history = false;
        let page = build(400, |v| v, |_| 30, &inputs);
        let toggle = page
            .lines
            .iter()
            .find_map(|l| match &l.row {
                Row::Toggle {
                    kind: Toggle::History,
                    on,
                    ..
                } => Some(*on),
                _ => None,
            })
            .unwrap();
        assert!(!toggle);
    }

    #[test]
    fn hotkeys_are_named_like_windows_writes_them() {
        assert_eq!(hotkey_name(0x1, 0x20), "Alt + Space");
        assert_eq!(hotkey_name(0x2 | 0x4, 0x4b), "Ctrl + Shift + K");
        assert_eq!(hotkey_name(0x8, 0x71), "Win + F2");
        assert_eq!(hotkey_name(0x1, 0x24), "Alt + Home");
        // Only the keys held so far, no key chosen yet.
        assert_eq!(hotkey_name(0x2, 0), "Ctrl");
        assert_eq!(hotkey_name(0, 0), "");
    }

    #[test]
    fn a_combo_needs_a_modifier_unless_it_is_a_function_key() {
        assert!(valid_combo(0x1, 0x20));
        assert!(valid_combo(0x2 | 0x8, 0x43));
        assert!(valid_combo(0, 0x70));
        assert!(valid_combo(0, 0x87));
        assert!(!valid_combo(0, 0x20));
        assert!(!valid_combo(0, 0x41));
    }
}
