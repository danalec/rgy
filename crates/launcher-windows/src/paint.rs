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

use rgy_launcher::results::IconKind;
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

/// The icon's size, in pixels: the shell's small icon.
const ICON_SIZE: i32 = 16;

/// The space the icon takes at the left of a row, the gap included.
///
/// The gutter belongs to the window and not to the model's geometry: the
/// icon is the window's own decoration, and the model's layout is the
/// same with an icon as without one.
const GUTTER: i32 = ICON_SIZE + 8;

/// The fonts the launcher draws with.
///
/// The fonts belong to the window and not to the paint: GDI refuses to
/// delete a font that is selected into a context, so a font made per
/// paint and deleted at the end of it is a font that is never deleted —
/// three handles per paint, on a launcher that paints on every
/// keystroke.
///
/// Segoe UI Variable Text is Win11's own; "Segoe UI" is the one every
/// Windows has, and it is the fallback when the variable face is not
/// there. The folder is a size smaller, and in the dimmer colour.
pub struct Fonts {
    field: HFONT,
    row: HFONT,
    folder: HFONT,
}

impl Fonts {
    /// The window's fonts, made once.
    pub fn new() -> Fonts {
        Fonts {
            field: font(17, FW_SEMIBOLD, "Segoe UI Variable Text"),
            row: font(15, FW_NORMAL, "Segoe UI Variable Text"),
            folder: font(13, FW_NORMAL, "Segoe UI Variable Text"),
        }
    }
}

impl Default for Fonts {
    fn default() -> Fonts {
        Fonts::new()
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

/// The icons, one per kind, resolved once.
///
/// The shell answers with a copy of the icon and the copy is the
/// caller's to destroy, so they are kept here and given back when the
/// window goes away. One icon per kind and not one per row: every
/// `.docx` on the disk has the same icon, and asking the shell is not
/// free.
pub struct Icons {
    icons: Vec<Option<HICON>>,
}

impl Icons {
    /// An empty cache: no icon has been asked for yet.
    pub fn new() -> Icons {
        Icons { icons: Vec::new() }
    }

    /// The icon of a kind, asked of the shell the first time and kept
    /// after that.
    pub fn get(&mut self, kind: IconKind) -> Option<HICON> {
        let slot = slot_of(kind);
        if self.icons.len() <= slot {
            self.icons.resize(slot + 1, None);
        }
        if let Some(icon) = self.icons[slot] {
            return Some(icon);
        }
        let icon = shell_icon(kind);
        self.icons[slot] = icon;
        icon
    }
}

impl Default for Icons {
    fn default() -> Icons {
        Icons::new()
    }
}

impl Drop for Icons {
    fn drop(&mut self) {
        for icon in self.icons.iter().flatten() {
            unsafe {
                DestroyIcon(*icon);
            }
        }
    }
}

/// Where a kind's icon is kept.
fn slot_of(kind: IconKind) -> usize {
    match kind {
        IconKind::Folder => 0,
        IconKind::Document => 1,
        IconKind::Image => 2,
        IconKind::Video => 3,
        IconKind::Audio => 4,
        IconKind::Application => 5,
        IconKind::File => 6,
    }
}

/// The shell's icon for a kind of file.
///
/// The path is a spelling and not a file: `SHGFI_USEFILEATTRIBUTES` makes
/// the shell answer from the spelling and the attributes alone, so
/// nothing here touches the disk.
///
/// The folder is the one that needs the attributes: the shell only
/// answers with the folder's icon when the file is said to be a folder,
/// and a drive's root is answered with the drive's icon whatever the
/// attributes say — so the folder's spelling is a plain name and not
/// `C:\`.
fn shell_icon(kind: IconKind) -> Option<HICON> {
    let (path, attributes): (&str, u32) = match kind {
        IconKind::Folder => ("folder", FILE_ATTRIBUTE_DIRECTORY),
        IconKind::Document => ("document.docx", FILE_ATTRIBUTE_NORMAL),
        IconKind::Image => ("image.png", FILE_ATTRIBUTE_NORMAL),
        IconKind::Video => ("video.mp4", FILE_ATTRIBUTE_NORMAL),
        IconKind::Audio => ("audio.mp3", FILE_ATTRIBUTE_NORMAL),
        IconKind::Application => ("application.exe", FILE_ATTRIBUTE_NORMAL),
        IconKind::File => ("file", FILE_ATTRIBUTE_NORMAL),
    };
    let wide: Vec<u16> = path.encode_utf16().chain(core::iter::once(0)).collect();
    let mut info = SHFILEINFOW {
        hIcon: core::ptr::null_mut(),
        iIcon: 0,
        dwAttributes: 0,
        szDisplayName: [0; 260],
        szTypeName: [0; 80],
    };
    let got = unsafe {
        SHGetFileInfoW(
            wide.as_ptr(),
            attributes,
            &mut info,
            core::mem::size_of::<SHFILEINFOW>() as u32,
            SHGFI_ICON | SHGFI_SMALLICON | SHGFI_USEFILEATTRIBUTES,
        )
    };
    if got == 0 || info.hIcon.is_null() {
        None
    } else {
        Some(info.hIcon)
    }
}

/// Draw an icon, at its own size.
fn draw_icon(hdc: HDC, icon: HICON, x: i32, y: i32) {
    unsafe {
        DrawIconEx(
            hdc,
            icon,
            x,
            y,
            ICON_SIZE,
            ICON_SIZE,
            0,
            core::ptr::null_mut(),
            DI_NORMAL,
        );
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
pub unsafe fn paint(
    hdc: HDC,
    view: &View,
    painted: &mut (i32, i32),
    backdrop: bool,
    icons: &mut Icons,
    fonts: &Fonts,
) {
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
        // The font the context had goes back when the paint is done: a
        // font that is still selected cannot be deleted, and the
        // window's fonts are deleted when the window dies.
        let stock = GetCurrentObject(hdc, OBJ_FONT);
        // The background: the system's backdrop is behind this window, and
        // painting over it would throw the blur away. Without a backdrop
        // there is nothing behind it, and the window paints its own
        // near-black.
        if !backdrop {
            fill(hdc, 0, 0, width, height, colour::BACKDROP);
        }

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
            fill(
                hdc,
                left + w + 2,
                field_top + 12,
                2,
                field_h - 26,
                colour::TEXT,
            );
        }

        // The status line, under the field.
        if !view.status.is_empty() {
            select(hdc, fonts.folder);
            draw_run(
                hdc,
                &view.status,
                left,
                field_top + field_h - 6,
                colour::DIM,
            );
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
            draw_row(hdc, fonts, row, left, top, row_h, content, icons);
        }

        // The actions menu, over the rows.
        if view.mode == Mode::Menu {
            draw_menu(hdc, fonts, view, width, height);
        }

        // The preview panel, over everything the rows were.
        if let Some(preview) = &view.preview {
            let top = layout.list_top() as i32;
            draw_preview(hdc, fonts, preview, left, top, width, height - top);
        }

        // The context is left as it was found.
        select(hdc, stock);
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

/// One row: the icon, the name with its match picked out, and the folder
/// under it.
///
/// The geometry is four numbers and three words, and a struct would be a
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
    icons: &mut Icons,
) {
    // The icon, in the gutter at the left of the row: what the row is,
    // asked of the shell and kept for the next row of the same kind.
    if let Some(icon) = icons.get(row.icon()) {
        draw_icon(hdc, icon, left, top + 5);
    }
    // The text, after the gutter.
    let text_left = left + GUTTER;
    let text_width = content.saturating_sub(GUTTER);
    {
        select(hdc, fonts.row);
        let name = row.name();
        let mut x = text_left;
        for (text, matched) in runs(name, row.matched()) {
            let colour = if matched { colour::MATCH } else { colour::TEXT };
            x += draw_run(hdc, &text, x, top + 5, colour);
            if x - text_left > text_width {
                break;
            }
        }
        // The second line: the folder in the name layer, the matched line
        // in the content layer.
        let context = row.context();
        if !context.is_empty() {
            select(hdc, fonts.folder);
            let text = shorten(context, 90);
            draw_run(hdc, &text, text_left, top + row_h - 17, colour::DIM);
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
///
/// The brush lives and dies here: `FillRect` takes it as an argument
/// and never selects it into the context, so nothing holds it after the
/// call and the delete is legal.
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
///
/// The brush is the rectangle's own and is given back with it, for the
/// same reason the fill's brush is.
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

/// Put a font in the context, and say what was there.
///
/// The old object is the caller's to keep: a font that is still selected
/// cannot be deleted, and the window's fonts are deleted when the window
/// dies.
fn select(hdc: HDC, font: HFONT) -> HANDLE {
    unsafe { SelectObject(hdc, font.cast()) }
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
