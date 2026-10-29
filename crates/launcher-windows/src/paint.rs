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

use alloc::collections::{BTreeMap, VecDeque};
use alloc::format;
use alloc::string::{String, ToString};
use alloc::vec;
use alloc::vec::Vec;

use rgy_launcher::layout::{SCALE_PERCENT, device_pixels};
use rgy_launcher::results::IconKind;
use rgy_launcher::{Layout, Mode, Preview, Row};

use crate::window::View;

use crate::win::*;

/// The window's own colours.
///
/// GDI's colours are `0x00BBGGRR`, so the bytes are in the order the
/// platform wants and not the order a person reads.
pub mod colour {
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

    /// The background's blue, on its own.
    ///
    /// The window that paints its own alpha has to know which pixels are
    /// the panel's own colour and which are the drawing on it, and it
    /// reads the bitmap — where the colours are in the order the
    /// platform stores them.
    pub const BLUE: u8 = 0x18;
    /// The background's green, on its own.
    pub const GREEN: u8 = 0x15;
    /// The background's red, on its own.
    pub const RED: u8 = 0x14;
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

/// One model unit, in the device pixels of this screen.
///
/// The drawing is in the model's units and the window is in device
/// pixels, and the two are the same number only on a 96-DPI screen.
fn px(scale: u32, units: i32) -> i32 {
    let scaled = device_pixels(units.unsigned_abs(), scale) as i32;
    if units < 0 { -scaled } else { scaled }
}

/// How many icons the cache keeps.
///
/// The shell is asked about a name, and the names a search walks through
/// are many: a hundred and twenty is more than a window shows at once,
/// so the icon the user is looking at is never one that went away.
const ICON_CACHE_LIMIT: usize = 128;

/// The face Win11 has: the variable one.
const VARIABLE_FACE: &str = "Segoe UI Variable Text";
/// The face every Windows has.
const FALLBACK_FACE: &str = "Segoe UI";

/// The fonts the launcher draws with.
///
/// The fonts belong to the window and not to the paint: GDI refuses to
/// delete a font that is selected into a context, so a font made per
/// paint and deleted at the end of it is a font that is never deleted —
/// three handles per paint, on a launcher that paints on every
/// keystroke.
///
/// The face is the platform's answer and not a constant: the variable
/// face is Win11's own, "Segoe UI" is the one every Windows has, and a
/// machine without the variable face draws with the other one. The
/// folder is a size smaller, and in the dimmer colour.
pub struct Fonts {
    field: HFONT,
    row: HFONT,
    folder: HFONT,
}

impl Fonts {
    /// The window's fonts, made once.
    ///
    /// The scale is in per cent (see [`SCALE_PERCENT`]): the heights are
    /// the model's, at the screen's own scale. An aware process does not
    /// scale fonts for you — `CreateFontW(17)` is seventeen device pixels
    /// whatever the monitor's DPI — so a font that was not scaled would
    /// be half the size on a screen twice as big.
    pub fn new(scale: u32) -> Fonts {
        let face = face();
        Fonts {
            field: font(font_height(17, scale), FW_SEMIBOLD, face),
            row: font(font_height(15, scale), FW_NORMAL, face),
            folder: font(font_height(13, scale), FW_NORMAL, face),
        }
    }
}

impl Default for Fonts {
    /// The fonts of a screen that is not scaled.
    fn default() -> Fonts {
        Fonts::new(SCALE_PERCENT)
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

/// The height of a font, in device pixels.
///
/// The font is a size in device pixels, and the size the window was
/// drawn with is the size of a 96-DPI screen: an aware process does not
/// scale fonts for you, so the height has to be scaled here.
fn font_height(units: i32, scale: u32) -> i32 {
    device_pixels(units.unsigned_abs(), scale) as i32
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

/// The face to ask for, given what the platform gave.
///
/// The variable face is Win11's own and the fallback is the one every
/// Windows has. A face the machine does not have comes back as another
/// one, and this is where the window picks the face it wants instead of
/// the one it got.
fn face_for(gave_the_variable_face: bool) -> &'static str {
    if gave_the_variable_face {
        VARIABLE_FACE
    } else {
        FALLBACK_FACE
    }
}

/// The face the window draws with.
///
/// The platform is asked once, when the window is made: the font is made
/// for the variable face, and `GetTextFaceW` answers with the face GDI
/// really gave. A machine without the variable face gives another one,
/// and the window takes the fallback — a window that asked for a face it
/// does not have would draw in a face nobody chose.
///
/// A platform that cannot be asked keeps the face the window wants: a
/// window that gave up its own face because it could not ask a question
/// would draw in a face nobody chose either.
fn face() -> &'static str {
    match platform_face(VARIABLE_FACE) {
        Some(given) => face_for(given.eq_ignore_ascii_case(VARIABLE_FACE)),
        None => VARIABLE_FACE,
    }
}

/// The face GDI gives for a face asked for.
///
/// The question is asked with a font made for the face and a context that
/// has it selected, and a face the machine does not have comes back as
/// another one. The font is made and given back — the window's own fonts
/// are made after this one, with the face this one reported.
fn platform_face(asked: &str) -> Option<String> {
    let probe = font(15, FW_NORMAL, asked);
    let given = face_of(probe);
    unsafe {
        DeleteObject(probe);
    }
    given
}

/// The face of a font, as the context that has it selected says it.
///
/// `GetTextFaceW` answers with the face that is really there: a font
/// asked for by a name the machine does not have is given another one,
/// and this is the only way to know which. The context is the screen's
/// own — the window has none of its own until it paints — and the font
/// that was there goes back in before the context is given back.
fn face_of(font: HFONT) -> Option<String> {
    unsafe {
        let hdc = GetDC(core::ptr::null_mut());
        if hdc.is_null() {
            return None;
        }
        let stock = select(hdc, font);
        let mut buf: [u16; 64] = [0; 64];
        let got = GetTextFaceW(hdc, buf.as_mut_ptr(), buf.len() as i32);
        select(hdc, stock);
        ReleaseDC(core::ptr::null_mut(), hdc);
        if got == 0 {
            return None;
        }
        let len = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
        Some(String::from_utf16_lossy(&buf[..len]))
    }
}

/// The icons, one per name, resolved once.
///
/// The shell answers with a copy of the icon and the copy is the
/// caller's to destroy, so they are kept here and given back when the
/// window goes away.
///
/// The cache is keyed by the name the shell was asked about, because
/// that is what the question was about — and it is bounded, because the
/// names a search walks through are many.
pub struct Icons {
    /// The icons, by the name they were asked for.
    ///
    /// A name the shell has no icon for is kept as a "none": the answer
    /// is the answer, and asking again would be asking the same
    /// question twice.
    icons: BTreeMap<String, Option<HICON>>,
    /// The names in the order they were asked for, so the oldest can go.
    order: VecDeque<String>,
    /// The icon of each kind, for the files the shell has none for.
    ///
    /// The kinds are few and the files are many: the icon of a kind is
    /// asked once and given to every file of that kind.
    kinds: BTreeMap<&'static str, Option<HICON>>,
}

impl Icons {
    /// An empty cache: no icon has been asked for yet.
    pub fn new() -> Icons {
        Icons {
            icons: BTreeMap::new(),
            order: VecDeque::new(),
            kinds: BTreeMap::new(),
        }
    }

    /// The icon of a row, asked of the shell the first time and kept
    /// after that.
    ///
    /// The shell is asked about the **file**: its own name, with its own
    /// extension, is what the shell looks up in the machine's own list of
    /// handlers — and a PDF with the reader's icon is the difference
    /// between a launcher that knows the machine and one that does not.
    pub fn get(&mut self, row: &Row) -> Option<HICON> {
        let kind = row.icon();
        let source = icon_source_for(row);
        let key = source.key();
        if let Some(icon) = self.icons.get(key.as_str()).copied() {
            // The name was asked before: the shell answered with the
            // file's own icon, or with none at all.
            return match icon {
                Some(icon) => Some(icon),
                None => self.kind_icon(kind),
            };
        }
        let icon = shell_icon(&source);
        self.remember(key, icon);
        match icon {
            Some(icon) => Some(icon),
            None => self.kind_icon(kind),
        }
    }

    /// Keep the answer, and make room for it.
    ///
    /// The cache holds the names the window drew, and a search walks
    /// through many of them: the limit is more than a window shows at
    /// once, so the icon the user is looking at is never one that went
    /// away. The icon that goes is given back — the shell's copy is the
    /// caller's to destroy.
    fn remember(&mut self, key: String, icon: Option<HICON>) {
        if self.order.len() >= ICON_CACHE_LIMIT
            && let Some(oldest) = self.order.pop_front()
            && let Some(Some(icon)) = self.icons.remove(&oldest)
        {
            unsafe {
                DestroyIcon(icon);
            }
        }
        self.order.push_back(key.clone());
        self.icons.insert(key, icon);
    }

    /// The icon of a kind, for a file the shell has none for.
    ///
    /// The kinds are few and the files are many, so the icon of a kind
    /// is asked once and given to every file of that kind. A folder has
    /// no fallback: a folder with a file's icon is the wrong picture of
    /// the thing.
    fn kind_icon(&mut self, kind: IconKind) -> Option<HICON> {
        if kind == IconKind::Folder {
            return None;
        }
        let name = kind_name(kind);
        if let Some(icon) = self.kinds.get(name).copied() {
            return icon;
        }
        let icon = shell_icon(&IconSource {
            name: name.to_string(),
            attributes: FILE_ATTRIBUTE_NORMAL,
        });
        self.kinds.insert(name, icon);
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
        for icon in self.icons.values().flatten() {
            unsafe {
                DestroyIcon(*icon);
            }
        }
        for icon in self.kinds.values().flatten() {
            unsafe {
                DestroyIcon(*icon);
            }
        }
    }
}

/// What the shell is asked about: a name, and the attributes it is said
/// to have.
struct IconSource {
    /// The name the shell is asked about.
    name: String,
    /// The attributes the shell is told about.
    attributes: u32,
}

impl IconSource {
    /// The key the icon is kept under.
    ///
    /// The key is the name the shell was asked about: the cache answers
    /// the question it was asked, and the question was about a name. The
    /// shell is the one that decides what a name means — it looks at the
    /// extension, and a name with no extension at all is a name of its
    /// own — and a cache that answered with another name's icon would be
    /// answering a question nobody asked.
    fn key(&self) -> String {
        self.name.clone()
    }
}

/// The name a folder is asked about.
///
/// A folder is asked about by a plain name and said to be a folder: the
/// shell only answers with the folder's icon when the file is said to be
/// a folder, and a drive's root is answered with the drive's icon
/// whatever the attributes say — so the folder's spelling is a plain
/// name and not `C:\`.
const FOLDER_NAME: &str = "folder";

/// The name the shell is asked about, for a row.
///
/// A file is asked about by its **own name**: the shell looks the
/// extension up in the machine's own list of handlers, and the name with
/// the right extension is what gets the icon the user expects — a PDF
/// with the reader's icon and not with Word's.
fn icon_source_for(row: &Row) -> IconSource {
    if row.icon() == IconKind::Folder {
        return IconSource {
            name: FOLDER_NAME.to_string(),
            attributes: FILE_ATTRIBUTE_DIRECTORY,
        };
    }
    IconSource {
        name: row.name().to_string(),
        attributes: FILE_ATTRIBUTE_NORMAL,
    }
}

/// The spelling the shell is asked about when it has no icon for the
/// file's own name.
///
/// It is the kind's own name: the shell knows the type of a file even
/// when it does not know the file.
fn kind_name(kind: IconKind) -> &'static str {
    match kind {
        IconKind::Folder => FOLDER_NAME,
        IconKind::Document => "document.docx",
        IconKind::Image => "image.png",
        IconKind::Video => "video.mp4",
        IconKind::Audio => "audio.mp3",
        IconKind::Application => "application.exe",
        IconKind::File => "file",
    }
}

/// The shell's icon for a name.
///
/// The path is a spelling and not a file: `SHGFI_USEFILEATTRIBUTES` makes
/// the shell answer from the spelling and the attributes alone, so
/// nothing here touches the disk.
fn shell_icon(source: &IconSource) -> Option<HICON> {
    let wide: Vec<u16> = source
        .name
        .encode_utf16()
        .chain(core::iter::once(0))
        .collect();
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
            source.attributes,
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

/// Draw an icon, at the size of the screen.
fn draw_icon(hdc: HDC, icon: HICON, x: i32, y: i32, size: i32) {
    unsafe {
        DrawIconEx(
            hdc,
            icon,
            x,
            y,
            size,
            size,
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
/// The scale is the screen's own, in per cent (see [`SCALE_PERCENT`]):
/// the picture is drawn in device pixels, and every number of the
/// drawing is a model unit at that scale.
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
    scale: u32,
) {
    let mut model = Layout::with_rows(view.rows.len() as u32);
    // The preview panel is a layer over the results, and it is as tall as
    // it needs to be: the host sized the window with the same geometry,
    // and the two have to agree or the panel would be cut off.
    if let Some(preview) = &view.preview {
        model.preview = preview.lines.len() as u32 + 1;
    }
    // The picture is the window's own size: the window's size is the
    // model's size at this screen's scale, and the two have to be the
    // same number of pixels or the picture does not fill the window.
    let width = device_pixels(model.width, scale) as i32;
    let height = device_pixels(model.height(), scale) as i32;
    *painted = (width, height);
    // The geometry inside the picture: every number of it is a model
    // unit, and every one of them is at this screen's scale.
    let layout = model.scaled(scale);

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
            let w = draw_run(
                hdc,
                &view.placeholder,
                left,
                field_top + px(scale, 14),
                colour::DIM,
            );
            let _ = w;
        } else {
            // The caret goes where the model says it is, and not after the
            // last character: the model moves it with the arrow keys, and
            // a paste lands where it is -- so a caret that is always at
            // the end puts the blinking line away from the text that
            // just arrived.
            let (before, after) = split_at_caret(&view.query, view.caret);
            let w = draw_run(hdc, before, left, field_top + px(scale, 14), colour::TEXT);
            // The caret: two pixels, two pixels after the text before it.
            fill(
                hdc,
                left + w + px(scale, 2),
                field_top + px(scale, 12),
                px(scale, 2),
                field_h - px(scale, 26),
                colour::TEXT,
            );
            // The text after the caret starts where the caret ends.
            draw_run(
                hdc,
                after,
                left + w + px(scale, 5),
                field_top + px(scale, 14),
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
                field_top + field_h - px(scale, 6),
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
                fill(hdc, 0, top, px(scale, 3), row_h, colour::ACCENT);
            }
            draw_row(hdc, fonts, row, left, top, row_h, content, icons, scale);
        }

        // The actions menu, over the rows.
        if view.mode == Mode::Menu {
            draw_menu(hdc, fonts, view, width, height, scale);
        }

        // The preview panel, over everything the rows were.
        if let Some(preview) = &view.preview {
            let top = layout.list_top() as i32;
            draw_preview(hdc, fonts, preview, left, top, width, height - top, &layout);
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
///
/// The geometry is the layout's own: the title, the header under it and
/// the spacing of the lines are the numbers the window's height was
/// computed from, and a painter with numbers of its own is a painter
/// that draws a panel the window did not ask for.
///
/// The scale is one of the numbers it is given, and a struct would be a
/// struct that only this function and its caller ever see.
#[allow(clippy::too_many_arguments)]
fn draw_preview(
    hdc: HDC,
    fonts: &Fonts,
    preview: &Preview,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    layout: &Layout,
) {
    if height <= 0 {
        return;
    }
    fill(hdc, 0, top, width, height, colour::BACKDROP);
    frame(hdc, 0, top, width, height, colour::BORDER);
    select(hdc, fonts.row);
    draw_run(
        hdc,
        &preview.title,
        left,
        top + layout.preview_title_top as i32,
        colour::TEXT,
    );
    select(hdc, fonts.folder);
    let line_height = layout.preview_line_height as i32;
    let mut y = top + layout.preview_header_height as i32;
    for line in &preview.lines {
        if y + line_height > top + height {
            break;
        }
        // The cut is in characters and not in pixels: it is a number of
        // letters, and a letter is a letter at any scale.
        let text = shorten(line, 90);
        draw_run(hdc, &text, left, y, colour::DIM);
        y += line_height;
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
    scale: u32,
) {
    // The icon, in the gutter at the left of row: what the row is,
    // asked of the shell and kept for the next row of the same type.
    if let Some(icon) = icons.get(row) {
        draw_icon(hdc, icon, left, top + px(scale, 5), px(scale, ICON_SIZE));
    }
    // The text, after the gutter.
    let gutter = px(scale, GUTTER);
    let text_left = left + gutter;
    let text_width = content.saturating_sub(gutter);
    {
        select(hdc, fonts.row);
        let name = row.name();
        let mut x = text_left;
        for (text, matched) in runs(name, row.matched()) {
            let colour = if matched { colour::MATCH } else { colour::TEXT };
            x += draw_run(hdc, &text, x, top + px(scale, 5), colour);
            if x - text_left > text_width {
                break;
            }
        }
        // The second line: the folder in the name layer, the matched line
        // in the content layer.
        let context = row.context();
        if !context.is_empty() {
            select(hdc, fonts.folder);
            // The cut is in characters and not in pixels: it is a number
            // of letters, and a letter is a letter at any scale.
            let text = shorten(context, 90);
            draw_run(
                hdc,
                &text,
                text_left,
                top + row_h - px(scale, 17),
                colour::DIM,
            );
        }
    }
}

/// The height of one entry of the actions menu, in the model's units.
const MENU_ROW_HEIGHT: i32 = 30;

/// The shortest an entry of the menu may be, in the model's units.
///
/// An entry holds a line of text and the margin under it, and an entry
/// shorter than that would cut the label in half.
const MENU_ROW_MINIMUM: i32 = 24;

/// The menu's own geometry.
///
/// The menu is a panel at the bottom of the window, and it is exactly as
/// tall as its entries: a row that is reserved and not drawn is a band
/// of nothing between the panel's edge and the first action, and it
/// reads as a fault in the drawing.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Menu {
    /// The top of the panel, in device pixels.
    top: i32,
    /// The height of the panel, in device pixels.
    height: i32,
    /// The height of one entry, in device pixels.
    row: i32,
}

impl Menu {
    /// The menu's geometry, in a window of this height.
    ///
    /// The entries are as tall as they can be and no taller: a window
    /// with one result is shorter than a menu of four entries at their
    /// full height, and the entries shrink to fit it. Only a window that
    /// cannot hold even the shortest entry has no menu to draw.
    fn at(entries: usize, window_height: i32, row: i32, shortest: i32) -> Option<Menu> {
        if entries == 0 {
            return None;
        }
        let row = (window_height / entries as i32).clamp(shortest, row);
        let height = row * entries as i32;
        if height > window_height {
            // The window has no room for the menu.
            return None;
        }
        Some(Menu {
            top: window_height - height,
            height,
            row,
        })
    }

    /// The top of one entry, in device pixels.
    fn entry_top(&self, index: usize) -> i32 {
        self.top + index as i32 * self.row
    }
}

/// The actions menu, drawn over the results.
fn draw_menu(hdc: HDC, fonts: &Fonts, view: &View, width: i32, height: i32, scale: u32) {
    if view.menu.is_empty() {
        return;
    }
    let row = px(scale, MENU_ROW_HEIGHT);
    let Some(menu) = Menu::at(view.menu.len(), height, row, px(scale, MENU_ROW_MINIMUM)) else {
        // The window has no room for the menu: the menu says so instead
        // of drawing half of itself.
        draw_too_short(hdc, fonts, width, height, scale);
        return;
    };
    // The menu's own panel: a little darker than the window, so it
    // reads as a layer above the results.
    fill(hdc, 0, menu.top, width, menu.height, colour::BACKDROP);
    frame(hdc, 0, menu.top, width, menu.height, colour::BORDER);
    for (i, action) in view.menu.iter().enumerate() {
        let y = menu.entry_top(i);
        let selected = i == view.menu_selected;
        if selected {
            fill(hdc, 0, y, width, menu.row, colour::SELECTED);
            fill(hdc, 0, y, px(scale, 3), menu.row, colour::ACCENT);
        }
        select(hdc, fonts.row);
        draw_run(
            hdc,
            action.label(),
            px(scale, 16),
            y + px(scale, 6),
            colour::TEXT,
        );
        select(hdc, fonts.folder);
        draw_run(
            hdc,
            action.shortcut(),
            width - px(scale, 90),
            y + px(scale, 7),
            colour::DIM,
        );
        select(hdc, fonts.row);
    }
}

/// What the menu says when the window is too short for it.
///
/// The menu is drawn over the results, and the window is as tall as the
/// model asked for: a window with one result is shorter than a menu of
/// four entries. A menu that cannot be drawn is not drawn half — it says
/// so, in the language the window speaks, because the model cannot know
/// how tall the window ended up.
const TOO_SHORT: &str = "a janela é curta demais para o menu";

/// The menu that does not fit.
fn draw_too_short(hdc: HDC, fonts: &Fonts, width: i32, height: i32, scale: u32) {
    // The panel is the whole window: there is no room for a panel at the
    // bottom, and a message without a panel reads as a line of the
    // results.
    fill(hdc, 0, 0, width, height, colour::BACKDROP);
    frame(hdc, 0, 0, width, height, colour::BORDER);
    select(hdc, fonts.folder);
    draw_run(
        hdc,
        TOO_SHORT,
        px(scale, 16),
        height / 2 - px(scale, 9),
        colour::DIM,
    );
}

/// The name, split into runs of matched and unmatched characters.
///
/// The span is a list of character offsets in the name — the first
/// character is 0 — and a fuzzy match's characters are not next to each
/// other, so the runs are what the eye sees as "the part that matched".
///
/// The offsets count characters and not bytes: an accent is two bytes
/// wide, and a mask of bytes picks out the wrong letters of every name
/// that has one in front of the match.
fn runs(name: &str, span: &[u32]) -> Vec<(String, bool)> {
    let chars: Vec<char> = name.chars().collect();
    let mut mask = vec![false; chars.len()];
    for &p in span {
        if let Some(i) = usize::try_from(p).ok()
            && i < mask.len()
        {
            mask[i] = true;
        }
    }
    let mut out: Vec<(String, bool)> = Vec::new();
    for (i, ch) in chars.iter().enumerate() {
        let matched = mask[i];
        match out.last_mut() {
            Some(last) if last.1 == matched => last.0.push(*ch),
            _ => out.push((ch.to_string(), matched)),
        }
    }
    out
}

/// The query, cut at the caret.
///
/// The caret is counted in characters and the cut is a byte offset, so
/// the cut has to land on a character boundary: half of an accent is not
/// a place a caret can be.
fn split_at_caret(query: &str, caret: usize) -> (&str, &str) {
    let at = query
        .char_indices()
        .nth(caret)
        .map(|(i, _)| i)
        .unwrap_or(query.len());
    (&query[..at], &query[at..])
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
    use core::ffi::c_void;

    use rgy_launcher::Action;

    use super::*;

    /// A surface to draw into, and the pixels that were drawn on it.
    ///
    /// The menu is a drawing, and the drawing is what the user sees: a
    /// test that only counts rows would not see the band of nothing the
    /// menu used to open with.
    struct Canvas {
        dc: HDC,
        bits: *mut u8,
        bitmap: HANDLE,
        /// The bitmap the context held before this one: a bitmap that is
        /// selected cannot be deleted, and a null handle does not
        /// deselect it.
        previous: HANDLE,
        width: i32,
    }

    impl Canvas {
        /// A surface of this size, in device pixels.
        fn new(width: i32, height: i32) -> Option<Canvas> {
            unsafe {
                let screen = GetDC(core::ptr::null_mut());
                if screen.is_null() {
                    return None;
                }
                let dc = CreateCompatibleDC(screen);
                ReleaseDC(core::ptr::null_mut(), screen);
                if dc.is_null() {
                    return None;
                }
                let info = BitmapInfo {
                    header: BitmapInfoHeader {
                        size: core::mem::size_of::<BitmapInfoHeader>() as u32,
                        width,
                        // The height is negative so that the first row of
                        // the bitmap is the top of the surface.
                        height: -height,
                        planes: 1,
                        bits: 32,
                        compression: BI_RGB,
                        image_size: 0,
                        x_pixels_per_meter: 0,
                        y_pixels_per_meter: 0,
                        colors_used: 0,
                        colors_important: 0,
                    },
                    colors: [0; 1],
                };
                let mut bits: *mut c_void = core::ptr::null_mut();
                let bitmap = CreateDIBSection(
                    dc,
                    &info,
                    DIB_RGB_COLORS,
                    &mut bits,
                    core::ptr::null_mut(),
                    0,
                );
                if bitmap.is_null() {
                    DeleteDC(dc);
                    return None;
                }
                let previous = SelectObject(dc, bitmap);
                Some(Canvas {
                    dc,
                    bits: bits.cast::<u8>(),
                    bitmap,
                    previous,
                    width,
                })
            }
        }

        /// The colour of a pixel, in the order the platform stores it.
        fn pixel(&self, x: i32, y: i32) -> (u8, u8, u8) {
            let stride = (self.width * 4) as usize;
            let at = (y as usize) * stride + (x as usize) * 4;
            unsafe {
                let pixel = self.bits.add(at);
                (*pixel, *pixel.add(1), *pixel.add(2))
            }
        }
    }

    impl Drop for Canvas {
        fn drop(&mut self) {
            unsafe {
                // The bitmap that is selected cannot be deleted: the
                // context goes back to the bitmap it held before it, and
                // a null handle does not deselect it.
                if !self.previous.is_null() {
                    SelectObject(self.dc, self.previous);
                }
                DeleteObject(self.bitmap);
                DeleteDC(self.dc);
            }
        }
    }

    /// A GDI colour in the order the platform stores the pixels in.
    ///
    /// GDI's colours are `0x00BBGGRR` and a 32-bit bitmap is `BGRA`, so
    /// the blue byte is the high one and the red byte is the low one.
    fn in_pixels(colour: u32) -> (u8, u8, u8) {
        ((colour >> 16) as u8, (colour >> 8) as u8, colour as u8)
    }

    #[test]
    fn a_name_with_no_match_is_one_run() {
        let r = runs("relatorio.txt", &[]);
        assert_eq!(r.len(), 1);
        assert_eq!(r[0].0, "relatorio.txt");
        assert!(!r[0].1);
    }

    /// The cut is on a character boundary.
    ///
    /// The caret is counted in characters and the text is cut in bytes,
    /// and half of an accent is not a place a caret can be.
    #[test]
    fn the_caret_cuts_between_characters() {
        let (before, after) = split_at_caret("faturação", 6);
        assert_eq!(before, "fatura");
        assert_eq!(after, "ção");
    }

    /// A caret past the end is the end.
    #[test]
    fn a_caret_past_the_end_is_the_end() {
        let (before, after) = split_at_caret("invoice", 99);
        assert_eq!(before, "invoice");
        assert_eq!(after, "");
    }

    #[test]
    fn a_match_splits_the_name_in_two() {
        // "rel" of "relatorio.txt" is the first three characters.
        let r = runs("relatorio.txt", &[0, 1, 2]);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "rel");
        assert!(r[0].1);
        assert_eq!(r[1].0, "atorio.txt");
        assert!(!r[1].1);
    }

    /// The span counts characters, and an accent is two bytes wide.
    ///
    /// "Relatório.txt" is thirteen characters and fourteen bytes, so
    /// the "txt" is at the characters 10, 11 and 12 — and at the bytes
    /// 11, 12 and 13. The span the engine sends is the first one, and a
    /// painter that counts bytes picks out ".tx" instead.
    #[test]
    fn a_match_after_an_accent_is_where_the_characters_are() {
        let r = runs("Relatório.txt", &[10, 11, 12]);
        assert_eq!(r.len(), 2);
        assert_eq!(r[0].0, "Relatório.");
        assert!(!r[0].1);
        assert_eq!(r[1].0, "txt");
        assert!(r[1].1);
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

    #[test]
    fn the_icon_is_the_file_s_own_name() {
        // The shell is asked about the file itself: a PDF asks with a
        // PDF's name, and not with a document's.
        let row = Row::new("C:\\Users\\dana\\relatorio.pdf".to_string(), 0);
        let source = icon_source_for(&row);
        assert_eq!(source.name, "relatorio.pdf");
        assert_eq!(source.attributes, FILE_ATTRIBUTE_NORMAL);
    }

    #[test]
    fn a_folder_asks_with_the_folder_s_own_name() {
        // A folder is a folder: the row says so, and the shell is asked
        // with the attributes that say it too.
        let row = Row::new("C:\\Users\\dana\\Docs".to_string(), 0).with_folder(true);
        let source = icon_source_for(&row);
        assert_eq!(source.name, FOLDER_NAME);
        assert_eq!(source.attributes, FILE_ATTRIBUTE_DIRECTORY);
    }

    /// The cache is keyed by the name the shell was asked about.
    ///
    /// The question is about the file's own name, so the answer is kept
    /// under that name: two PDFs are two questions, and two files with
    /// no extension at all are two questions as well. A cache keyed by
    /// the extension gave every extension-less file the icon of the first
    /// one that was asked about.
    #[test]
    fn two_files_with_the_same_extension_do_not_share_one_icon() {
        let one = icon_source_for(&Row::new("C:/a/relatorio.pdf".to_string(), 0));
        let other = icon_source_for(&Row::new("C:/b/contrato.pdf".to_string(), 0));
        assert_ne!(one.key(), other.key());
        // And two files with no extension are two questions as well.
        let readme = icon_source_for(&Row::new("C:/a/README".to_string(), 0));
        let makefile = icon_source_for(&Row::new("C:/b/Makefile".to_string(), 0));
        assert_ne!(readme.key(), makefile.key());
        // And a folder is not a file.
        let folder = icon_source_for(&Row::new("C:/a/Docs".to_string(), 0).with_folder(true));
        assert_ne!(one.key(), folder.key());
    }

    /// The window falls back to the face every Windows has.
    ///
    /// The platform cannot be asked about a machine that is not this one,
    /// so the decision is proved where it is made: the face the window
    /// asks for is the face the platform gave, and a machine without the
    /// variable face asks for the fallback.
    #[test]
    fn a_machine_without_the_variable_face_gets_the_fallback() {
        assert_eq!(face_for(false), FALLBACK_FACE);
        assert_eq!(face_for(true), VARIABLE_FACE);
        assert_ne!(face_for(false), VARIABLE_FACE);
    }

    /// The face the window ends up with is the face it asked for.
    ///
    /// The platform cannot be asked in a test — GDI has no fonts to give
    /// where the suite runs — so what is proved here is the decision:
    /// the window asks for the variable face where the platform has it,
    /// for the fallback where it does not, and for nothing else. Where
    /// the platform does answer, the answer is the face the window
    /// asks for.
    #[test]
    fn the_face_the_window_ends_up_with_is_the_face_it_asked_for() {
        let asked = face();
        match platform_face(VARIABLE_FACE) {
            // The platform answered: the face the window asks for is the
            // face the platform gave.
            Some(given) => assert_eq!(asked, face_for(given.eq_ignore_ascii_case(VARIABLE_FACE))),
            // The platform cannot be asked: the window keeps the face it
            // wants, and says which one that is.
            None => assert_eq!(asked, VARIABLE_FACE),
        }
    }

    /// The cache does not grow without bound.
    ///
    /// The shell is asked about a name, and the names a search walks
    /// through are many: the cache keeps a bounded number of them, and
    /// the ones that go are given back.
    #[test]
    fn the_cache_does_not_grow_without_bound() {
        let mut icons = Icons::new();
        for i in 0..(ICON_CACHE_LIMIT + 10) {
            let row = Row::new(format!("C:/a/file{i}.txt"), 0);
            icons.get(&row);
        }
        assert!(
            icons.icons.len() <= ICON_CACHE_LIMIT,
            "the cache kept {} icons",
            icons.icons.len()
        );
    }

    /// The fonts are the size of the screen they are drawn on.
    ///
    /// An aware process does not scale fonts for you: `CreateFontW(17)`
    /// is seventeen device pixels whatever the monitor's DPI, so a font
    /// that was not scaled would be half the size on a screen twice as
    /// big.
    #[test]
    fn the_fonts_are_the_size_of_the_screen() {
        assert_eq!(font_height(17, SCALE_PERCENT), 17);
        assert_eq!(font_height(15, SCALE_PERCENT), 15);
        assert_eq!(font_height(13, SCALE_PERCENT), 13);
        assert_eq!(font_height(17, SCALE_PERCENT * 2), 34);
        assert_eq!(font_height(15, SCALE_PERCENT * 2), 30);
        assert_eq!(font_height(13, SCALE_PERCENT * 2), 26);
    }

    /// The drawing's own numbers are at the screen's scale too.
    ///
    /// The icon and the gutter are the window's own decoration, and they
    /// are model units like everything else.
    #[test]
    fn the_drawing_is_at_the_screen_s_scale() {
        assert_eq!(px(SCALE_PERCENT, ICON_SIZE), 16);
        assert_eq!(px(SCALE_PERCENT * 2, ICON_SIZE), 32);
        assert_eq!(px(SCALE_PERCENT, GUTTER), 24);
        assert_eq!(px(SCALE_PERCENT * 2, GUTTER), 48);
    }

    /// The menu opens with its first entry.
    ///
    /// The panel is exactly as tall as its entries, so the first row of
    /// the panel is the first entry's own: a row that is reserved and
    /// not drawn is a band of nothing between the panel's edge and the
    /// first action, and it reads as a fault in the drawing.
    #[test]
    fn the_menu_opens_with_its_first_entry() {
        let Some(canvas) = Canvas::new(300, 200) else {
            panic!("the platform gave no surface to draw on");
        };
        let fonts = Fonts::new(SCALE_PERCENT);
        let view = View {
            menu: vec![
                Action::Open,
                Action::OpenFolder,
                Action::CopyPath,
                Action::Preview,
            ],
            menu_selected: 0,
            ..View::default()
        };
        // The surface is the window's own colour, so the panel's edge
        // is the first thing on it that is not.
        fill(canvas.dc, 0, 0, 300, 200, colour::BACKDROP);
        draw_menu(canvas.dc, &fonts, &view, 300, 200, SCALE_PERCENT);
        let top = (0..200)
            .find(|&y| canvas.pixel(10, y) != in_pixels(colour::BACKDROP))
            .expect("the menu drew a panel");
        assert_eq!(
            canvas.pixel(10, top),
            in_pixels(colour::SELECTED),
            "the panel opens with its first entry"
        );
        unsafe {
            // The font that is still selected cannot be deleted: the
            // stock one goes back in before the window's fonts go.
            let stock = GetCurrentObject(canvas.dc, OBJ_FONT);
            SelectObject(canvas.dc, stock);
        }
    }

    /// The menu is exactly as tall as its entries.
    ///
    /// A row that is reserved and not drawn is a band of nothing between
    /// the panel's edge and the first action, and it reads as a fault in
    /// the drawing.
    #[test]
    fn the_menu_is_as_tall_as_its_entries() {
        let menu = Menu::at(4, 300, 30, 24).expect("the menu fits");
        assert_eq!(menu.height, 4 * 30);
        assert_eq!(menu.top, 300 - 4 * 30);
        assert_eq!(menu.entry_top(0), menu.top, "the first entry is the top");
        assert_eq!(menu.entry_top(3), menu.top + 3 * 30);
    }

    /// A window with one result is shorter than a menu of four entries,
    /// and the entries shrink to fit it.
    #[test]
    fn the_menu_shrinks_to_fit_a_short_window() {
        let menu = Menu::at(4, 110, 30, 24).expect("the menu still fits");
        assert!(menu.height <= 110, "the menu is inside the window");
        assert_eq!(menu.row, 27);
        assert_eq!(
            menu.entry_top(3) + menu.row,
            110,
            "and the last entry ends at the bottom"
        );
    }

    /// A window with no room for the menu has no menu: the menu says so
    /// instead of drawing half of itself.
    #[test]
    fn a_window_with_no_room_for_the_menu_has_no_menu() {
        assert_eq!(Menu::at(4, 80, 30, 24), None);
    }

    /// The preview's lines fit in the panel the window asked for.
    ///
    /// The painter and the layout have to agree: a panel shorter than
    /// its own text cuts the last line off, and a panel taller is a
    /// hundred units of nothing under the text.
    #[test]
    fn the_preview_fits_in_the_window_the_layout_asked_for() {
        let mut model = Layout::with_rows(1);
        // A title and thirteen lines under it.
        model.preview = 14;
        let window = model.height();
        let panel = window - model.list_top();
        assert_eq!(
            panel,
            model.preview_height() + model.padding + model.border,
            "the panel is the text and the window's own margin"
        );
        let last = model.preview_header_height + 12 * model.preview_line_height;
        assert!(
            last + model.preview_line_height <= panel,
            "the last line of the panel is inside it"
        );
    }

    #[test]
    fn the_key_is_the_name_the_shell_was_asked_about() {
        // The name is the question, and the case is part of it.
        let one = icon_source_for(&Row::new("C:/a/Relatorio.PDF".to_string(), 0));
        let other = icon_source_for(&Row::new("C:/b/relatorio.pdf".to_string(), 0));
        assert_eq!(one.key(), "Relatorio.PDF");
        assert_eq!(other.key(), "relatorio.pdf");
    }
}
