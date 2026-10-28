// SPDX-License-Identifier: Apache-2.0

//! The painting: GDI, by hand.
//!
//! The launcher draws a field, a list, and nothing else. The list is a name
//! in a bright colour with the matched characters picked out in a warmer
//! one, and the folder under it in a dimmer colour and a smaller size —
//! the name is what the user is looking for and the folder is the context
//! that says whether they found it.
//!
//! The text is drawn in runs: a run of matched characters, a run of
//! unmatched ones, and so on. GDI draws one colour at a time, so a name
//! with a match in the middle of it is drawn in pieces, each measured
//! before it is placed.

use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use rgy_launcher::{Layout, Mode, Preview, Row};

use crate::window::View;

use crate::win::*;

/// The window's own colours.
///
/// GDI's colours are `0x00BBGGRR`, so the bytes are in the order the
/// platform wants and not the order a person reads.
mod colour {
    /// The query, and a name: almost white, a touch warm.
    pub const TEXT: u32 = 0x00F5_F2F0;
    /// The context: the folder, the placeholder.
    pub const DIM: u32 = 0x0091_8782;
    /// The matched characters: the accent.
    pub const MATCH: u32 = 0x00FF_B478;
    /// The selected row's background.
    pub const SELECTED: u32 = 0x002E_2826;
    /// The bar on the left of the selected row.
    pub const ACCENT: u32 = 0x00FF_965A;
    /// The window's own background, when there is no backdrop.
    pub const BACKDROP: u32 = 0x0018_1514;
    /// The border: a hairline, a little lighter than the background.
    pub const BORDER: u32 = 0x0042_3C3A;
}

const DT_LEFT: u32 = 0x0000_0000;
const DT_SINGLELINE: u32 = 0x0000_0020;
const DT_NOPREFIX: u32 = 0x0000_0800;
const DT_END_ELLIPSIS: u32 = 0x0000_8000;

/// The fonts the launcher draws with.
///
/// Segoe UI Variable Text is Win11's own; "Segoe UI" is the one every
/// Windows has, and it is the fallback when the variable face is not
/// there. The folder is a size smaller, and in the dimmer colour.
struct Fonts {
    field: HFONT,
    row: HFONT,
    folder: HFONT,
}

impl Fonts {
    fn new() -> Fonts {
        Fonts {
            field: font(17, FW_SEMIBOLD, "Segoe UI Variable Text"),
            row: font(15, FW_NORMAL, "Segoe UI Variable Text"),
            folder: font(13, FW_NORMAL, "Segoe UI Variable Text"),
        }
    }
}

impl Drop for Fonts {
    fn drop(&mut self) {
        unsafe {
            DeleteObject(self.field);
            DeleteObject(self.row);
            DeleteObject(self.folder);
        }
    }
}

/// One font, at a size and a weight.
fn font(height: i32, weight: i32, face: &str) -> HFONT {
    let mut name: Vec<u16> = face.encode_utf16().collect();
    name.resize(32, 0);
    unsafe {
        CreateFontW(
            height,
            0,
            0,
            0,
            weight,
            0,
            0,
            0,
            DEFAULT_CHARSET as u32,
            OUT_DEFAULT_PRECIS as u32,
            CLIP_DEFAULT_PRECIS as u32,
            CLEARTYPE_QUALITY as u32,
            (DEFAULT_PITCH | FF_DONTCARE) as u32,
            name.as_ptr(),
        )
    }
}

/// Draw the window.
///
/// The device context is a raw pointer: the caller has to have one, and
/// the function that owns the window is the only caller there is.
///
/// # Safety
///
/// `hdc` must be a device context the thread may draw on.
pub unsafe fn paint(hdc: HDC, view: &View, painted: &mut (i32, i32)) {
    let fonts = Fonts::new();
    let mut layout = Layout::with_rows(view.rows.len() as u32);
    // The preview panel is a layer over the results, and it is as tall as
    // it needs to be: the host sized the window with the same geometry,
    // and the two have to agree or the panel would be cut off.
    if let Some(preview) = &view.preview {
        layout.preview = preview.lines.len() as u32 + 1;
    }
    let width = layout.width as i32;
    let height = layout.height() as i32;
    *painted = (width, height);

    unsafe {
        SetBkMode(hdc, TRANSPARENT);
        // The background: the system's backdrop is behind this window, and
        // painting over it would throw the blur away.
        fill(hdc, 0, 0, width, height, colour::BACKDROP);

        // The border: a hairline around the whole window.
        frame(hdc, 0, 0, width, height, colour::BORDER);

        let left = layout.content_left() as i32;
        let content = layout.content_width() as i32;

        // The field.
        let field_top = layout.input_top() as i32;
        let field_h = layout.input_height as i32;
        select(hdc, fonts.field);
        if view.query.is_empty() {
            let w = draw_run(hdc, &view.placeholder, left, field_top + 14, colour::DIM);
            let _ = w;
        } else {
            let w = draw_run(hdc, &view.query, left, field_top + 14, colour::TEXT);
            // The caret: two pixels after the last character.
            fill(hdc, left + w + 2, field_top + 12, 2, field_h - 26, colour::TEXT);
        }

        // The status line, under the field.
        if !view.status.is_empty() {
            select(hdc, fonts.folder);
            draw_run(hdc, &view.status, left, field_top + field_h - 6, colour::DIM);
        }

        // The rows.
        let visible = view.rows.len();
        for (i, row) in view.rows.iter().enumerate().take(visible) {
            let top = layout.row_top(i as u32) as i32;
            let row_h = layout.row_height as i32;
            if i == view.selected {
                fill(hdc, 0, top, width, row_h, colour::SELECTED);
                // The accent bar on the left: the selection's own edge.
                fill(hdc, 0, top, 3, row_h, colour::ACCENT);
            }
            draw_row(hdc, &fonts, row, left, top, row_h, content);
        }

        // The actions menu, over the rows.
        if view.mode == Mode::Menu {
            draw_menu(hdc, &fonts, view, width, height);
        }

        // The preview panel, over everything the rows were.
        if let Some(preview) = &view.preview {
            let top = layout.list_top() as i32;
            draw_preview(hdc, &fonts, preview, left, top, width, height - top);
        }
    }
}

/// The preview panel: the file's name, and the lines under it.
///
/// The panel is the window's own background with a hairline around it, so
/// it reads as a sheet of paper on top of the list rather than as a
/// different window.
fn draw_preview(
    hdc: HDC,
    fonts: &Fonts,
    preview: &Preview,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
) {
    if height <= 0 {
        return;
    }
    fill(hdc, 0, top, width, height, colour::BACKDROP);
    frame(hdc, 0, top, width, height, colour::BORDER);
    select(hdc, fonts.row);
    draw_run(hdc, &preview.title, left, top + 5, colour::TEXT);
    select(hdc, fonts.folder);
    let mut y = top + 34;
    for line in &preview.lines {
        if y + 18 > top + height {
            break;
        }
        let text = shorten(line, 90);
        draw_run(hdc, &text, left, y, colour::DIM);
        y += 18;
    }
}

/// One row: the name, with its match picked out, and the folder under it.
///
/// The geometry is four numbers and two words, and a struct would be a
/// struct that only this function and its caller ever see.
#[allow(clippy::too_many_arguments)]
fn draw_row(
    hdc: HDC,
    fonts: &Fonts,
    row: &Row,
    left: i32,
    top: i32,
    row_h: i32,
    content: i32,
) {
    {
        select(hdc, fonts.row);
        let name = row.name();
        let mut x = left;
        for (text, matched) in runs(name, row.matched()) {
            let colour = if matched { colour::MATCH } else { colour::TEXT };
            x += draw_run(hdc, &text, x, top + 5, colour);
            if x - left > content {
                break;
            }
        }
        // The second line: the folder in the name layer, the matched line
        // in the content layer.
        let context = row.context();
        if !context.is_empty() {
            select(hdc, fonts.folder);
            let text = shorten(context, 90);
            draw_run(hdc, &text, left, top + row_h - 17, colour::DIM);
        }
    }
}

/// The actions menu, drawn over the results.
fn draw_menu(hdc: HDC, fonts: &Fonts, view: &View, width: i32, height: i32) {
    {
        let h = 30;
        let top = height - (view.menu.len() as i32 + 1) * h;
        // The menu's own panel: a little darker than the window, so it
        // reads as a layer above the results.
        fill(hdc, 0, top, width, height - top, colour::BACKDROP);
        frame(hdc, 0, top, width, height - top, colour::BORDER);
        for (i, action) in view.menu.iter().enumerate() {
            let y = top + (i as i32 + 1) * h;
            let selected = i == view.menu_selected;
            if selected {
                fill(hdc, 0, y - 4, width, h, colour::SELECTED);
                fill(hdc, 0, y - 4, 3, h, colour::ACCENT);
            }
            select(hdc, fonts.row);
            draw_run(hdc, action.label(), 16, y + 6, colour::TEXT);
            select(hdc, fonts.folder);
            draw_run(hdc, action.shortcut(), width - 90, y + 7, colour::DIM);
            select(hdc, fonts.row);
        }
    }
}

/// The name, split into runs of matched and unmatched characters.
///
/// The span is a list of byte offsets in the name, and a fuzzy match's
/// characters are not next to each other — so the runs are what the eye
/// sees as "the part that matched".
fn runs(name: &str, span: &[u32]) -> Vec<(String, bool)> {
    let mut mask = vec![false; name.len()];
    for &p in span {
        if let Some(i) = usize::try_from(p).ok()
            && i < mask.len()
        {
            mask[i] = true;
        }
    }
    let mut out: Vec<(String, bool)> = Vec::new();
    for (i, ch) in name.char_indices() {
        let len = ch.len_utf8();
        let matched = mask[i..i + len].iter().any(|&b| b);
        match out.last_mut() {
            Some(last) if last.1 == matched => last.0.push(ch),
            _ => out.push((ch.to_string(), matched)),
        }
    }
    out
}

/// Draw a run of text, and say how wide it was.
fn draw_run(hdc: HDC, text: &str, x: i32, y: i32, colour: u32) -> i32 {
    let w: Vec<u16> = text.encode_utf16().collect();
    if w.is_empty() {
        return 0;
    }
    unsafe {
        let mut size = POINT { x: 0, y: 0 };
        GetTextExtentPoint32W(hdc, w.as_ptr(), w.len() as i32, &mut size);
        SetTextColor(hdc, colour);
        let mut r = RECT {
            left: x,
            top: y,
            right: x + size.x,
            bottom: y + size.y,
        };
        DrawTextW(
            hdc,
            w.as_ptr(),
            w.len() as i32,
            &mut r,
            DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
        size.x
    }
}

/// Fill a rectangle.
fn fill(hdc: HDC, x: i32, y: i32, w: i32, h: i32, colour: u32) {
    unsafe {
        let brush = CreateSolidBrush(colour);
        let r = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };
        FillRect(hdc, &r, brush);
        DeleteObject(brush);
    }
}

/// Draw a hairline around a rectangle.
fn frame(hdc: HDC, x: i32, y: i32, w: i32, h: i32, colour: u32) {
    unsafe {
        let brush = CreateSolidBrush(colour);
        let r = RECT {
            left: x,
            top: y,
            right: x + w,
            bottom: y + h,
        };
        FrameRect(hdc, &r, brush);
        DeleteObject(brush);
    }
}

/// Put a font in the context.
fn select(hdc: HDC, font: HFONT) {
    unsafe {
        SelectObject(hdc, font.cast());
    }
}

/// Shorten a path for the line it is drawn on.
fn shorten(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let head: String = s.chars().skip(s.chars().count() - max).collect();
    format!("…{head}")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_name_with_no_match_is_one_run() {
        let r = runs("relatorio.txt", &[]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, "relatorio.txt");
        assert!(!r[0].1);
    }

    #[test]
    fn a_match_splits_the_name_in_two() {
        // "rel" of "relatorio.txt" is the first three bytes.
        let r = runs("relatorio.txt", &[0, 1, 2]);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "rel");
        assert!(r[0].1);
        assert_eq!(r[1].0, "atorio.txt");
        assert!(!r[1].1);
    }

    #[test]
    fn a_scattered_match_makes_many_runs() {
        let r = runs("abcdef", &[0, 2, 4]);
        assert_eq!(r.len(), 6);
        assert!(r[0].1 && !r[1].1 && r[2].1 && !r[3].1 && r[4].1 && !r[5].1);
    }

    #[test]
    fn a_span_past_the_end_is_ignored() {
        // The span comes from the engine and the name comes from the
        // path; a mismatch between them must not panic.
        let r = runs("abc", &[0, 99]);
        assert_eq!(r.len(), 2);
        assert!(r[0].1);
    }

    #[test]
    fn a_long_path_is_shortened_at_the_front() {
        let s = shorten(&"x".repeat(200), 10);
        assert_eq!(s.chars().count(), 11);
        assert!(s.starts_with('…'));
    }
}
