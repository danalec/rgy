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

use rgy_launcher::latency::Latency;
use rgy_launcher::layout::{
    PANEL_INSET, SCALE_PERCENT, SIDE_BAR_ENTRY_HEIGHT, SIDE_BAR_MINIMUM_ENTRY_HEIGHT,
    SIDE_BAR_WIDTH, device_pixels,
};
use rgy_launcher::results::IconKind;
use rgy_launcher::text::{Lang, Text};
use rgy_launcher::theme::Theme;
use rgy_launcher::{Layout, Mode, Preview, Row};

use crate::window::View;

use crate::win::*;

/// The colours one paint is painted in.
///
/// GDI's colours are `0x00BBGGRR`, so the bytes are in the order the
/// platform wants and not the order a person reads. They were constants
/// of the paint once, and a constant is a colour a theme cannot reach:
/// the colours are the paint's parameters now, handed down by the window
/// that holds the theme, and [`Colours::of`] is where a theme becomes
/// them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Colours {
    /// The query, and a name: what the user is looking for.
    pub text: u32,
    /// The context: the folder, the placeholder, the line under the
    /// field.
    pub dim: u32,
    /// The matched characters: what the query earned.
    pub match_: u32,
    /// The selected row's background.
    pub selected: u32,
    /// The machine's own mark: the bar on the left of the selected row,
    /// and the column's hairline while the menu has the keyboard.
    pub accent: u32,
    /// The time the answers take: the number and the chart.
    pub latency: u32,
    /// The window's own background, when there is no backdrop.
    pub backdrop: u32,
    /// The border: a hairline, a little lighter than the background.
    pub border: u32,
    /// The background's blue, on its own.
    ///
    /// The window that paints its own alpha has to know which pixels are
    /// the panel's own colour and which are the drawing on it, and it
    /// reads the bitmap — where the colours are separate channels and
    /// not one word of the platform's own order.
    pub background_blue: u8,
    /// The background's green, on its own, for the alpha composition.
    pub background_green: u8,
    /// The background's red, on its own, for the alpha composition.
    pub background_red: u8,
}

impl Colours {
    /// The window's own palette, the one it was drawn in before themes
    /// existed: warm near-whites on a warm near-black, the blue of the
    /// matches and the green of the meter — the terminal's own green, so
    /// the two faces say the same thing in the same colour.
    ///
    /// The tests that pin today's rendering draw with these, and the
    /// theme's first duty is to reproduce them: a theme that changed the
    /// colours nobody asked about is a theme that redraws the launcher
    /// under its users.
    pub const WARM: Colours = Colours {
        text: 0x00F5_F2F0,
        dim: 0x0091_8782,
        match_: 0x00FF_B478,
        selected: 0x002E_2826,
        accent: 0x00FF_965A,
        latency: 0x008B_D977,
        backdrop: 0x0018_1514,
        border: 0x0042_3C3A,
        background_blue: 0x18,
        background_green: 0x15,
        background_red: 0x14,
    };

    /// The colours of a theme, in the order the platform takes them.
    ///
    /// Each role becomes the one word GDI wants (`0x00BBGGRR`, by
    /// `Rgb::bgr`), and the background's channels go on their own as
    /// well: the composition that works out the panel's alpha compares
    /// the channels one by one, and a word of the platform's order is a
    /// word it would have to take apart again.
    pub fn of(theme: &Theme) -> Colours {
        Colours {
            text: theme.text.bgr(),
            dim: theme.dim.bgr(),
            match_: theme.matched.bgr(),
            selected: theme.selected.bgr(),
            accent: theme.accent.bgr(),
            latency: theme.good.bgr(),
            backdrop: theme.background.bgr(),
            border: theme.border.bgr(),
            background_blue: theme.background.b,
            background_green: theme.background.g,
            background_red: theme.background.r,
        }
    }
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
        let kind = row.icon().kind;
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
    if row.icon().kind == IconKind::Folder {
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

/// The shell writes the whole of `SHFILEINFOW`, so its layout is fixed
/// at COMPILE TIME.
///
/// A struct the platform writes is the platform's struct, whole: a
/// truncated one is a buffer the platform writes past, and the size
/// handed to `SHGetFileInfoW` has to be the platform's own. A runtime
/// assertion protects one host, and only while the tests happen to run;
/// these fail the build on every host the crate is compiled for. The
/// notification-area structs are asserted beside their definitions in
/// `win.rs`; this one is asserted beside the call that fills it in.
const _: () = assert!(core::mem::size_of::<SHFILEINFOW>() == 696);
const _: () = assert!(core::mem::offset_of!(SHFILEINFOW, hIcon) == 0);
const _: () = assert!(core::mem::offset_of!(SHFILEINFOW, szDisplayName) == 16);
const _: () = assert!(core::mem::offset_of!(SHFILEINFOW, szTypeName) == 536);

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
            x,
            y,
            icon,
            size,
            size,
            0,
            core::ptr::null_mut(),
            DI_NORMAL,
        );
    }
}

/// The geometry of a picture, measured from the view it draws.
///
/// One place answers "how big is this picture", for the picture and for
/// the surface it is drawn on: the two disagreed once — the settings
/// panel was a list of its own, and a picture measured only from the rows
/// gave the panel no area, so the window grew and the panel was drawn
/// nowhere while the arrows adjusted it.
///
/// The host sizes the window with the model's own [`Layout`], and the
/// numbers here are the same ones: a preview is as tall as its lines, a
/// settings panel is as tall as its rows, and the column of actions is
/// inside the room the results leave.
fn model_of(view: &View) -> Layout {
    // The view carries the width the host sized the window with, and the
    // picture is measured from it: the old code started from the
    // geometry's default (600 units) and never applied it, so a window
    // the host made wider or narrower was drawn 600 units wide — the
    // query, the row bands and the actions column all positioned for a
    // width that was not on the screen, and the unpainted strip turned
    // into a solid bar once the compositor gave it full alpha.
    //
    // A view that has not been sized yet (the window's first, hidden
    // frame and a test's default) keeps the geometry's own width, so the
    // picture is never drawn zero units wide.
    let width = if view.width == 0 {
        Layout::default().width
    } else {
        view.width
    };
    let mut model = Layout::with_rows(view.rows.len() as u32).with_width(width);
    // The preview panel is a layer over the results, and it is as tall as
    // it needs to be: the host sized the window with the same geometry,
    // and the two have to agree or the panel would be cut off.
    if let Some(preview) = &view.preview {
        model.preview = preview.lines.len() as u32 + 1;
    }
    // The settings panel is a list of its own, and it brings its own
    // room: a command line has no results, and a picture measured from
    // the rows would have no area to draw the panel in.
    if view.mode == Mode::Settings {
        model.panel = view.settings.len() as u32;
    }
    // The actions are a column on the right of the window: the results
    // give up the room of it and the window does not grow — a window
    // that grows when the actions open is a window that moves under the
    // user's hands.
    //
    // The room is the geometry's rule and not the picture's: the model
    // asked the same question when it sized the window, and the two have
    // to answer it the same way or the column is drawn over the names.
    // In `Mode::Settings` the panel holds the keyboard and owns the
    // whole content width: the column there would be an inert legend
    // drawn over the panel and taking its width. `App::layout` asks the
    // same question, and the model and the picture answer it the same
    // way.
    if view.mode != Mode::Settings && model.column_fits(view.menu.len() as u32) {
        model = model.with_side_bar(SIDE_BAR_WIDTH);
    }
    model
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
/// The language is the one the window is speaking: the menu's labels
/// and the column's own sentence come from the vocabulary, and a window
/// that drew the labels in another language would be a window the user
/// reads in two languages at once.
///
/// The colours are the paint's parameters and not the paint's own: the
/// window holds the theme and hands its colours down, so a window whose
/// theme changed draws the same picture in another palette and nothing
/// else of the drawing moves.
///
/// The picture is a context, a view, a few numbers, a palette and a
/// language, and a struct would be a struct that only this function and
/// its caller ever see.
///
/// The answer is the picture's size in device pixels: the caller asks
/// whether it covers the surface it is presented on. It is a returned
/// value and not stored state — a field written every frame and read
/// nowhere only looks like somebody used it.
///
/// # Safety
///
/// `hdc` must be a device context the thread may draw on.
#[allow(clippy::too_many_arguments)]
pub unsafe fn paint(
    hdc: HDC,
    view: &View,
    backdrop: bool,
    icons: &mut Icons,
    fonts: &Fonts,
    colours: &Colours,
    scale: u32,
    lang: Lang,
) -> (i32, i32) {
    let model = model_of(view);
    let column = model.side_bar != 0;
    // The picture is the window's own size: `model_of` measured it from
    // the view's width — the same number the host sized the window from —
    // so at this screen's scale the two are the same number of pixels and
    // the picture fills the window. A picture that did not fill it would
    // leave a strip the compositor turns into a solid bar.
    let width = device_pixels(model.width, scale) as i32;
    let height = device_pixels(model.height(), scale) as i32;
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
            fill(hdc, 0, 0, width, height, colours.backdrop);
        }

        // The border: a hairline around the whole window.
        frame(hdc, 0, 0, width, height, colours.border);

        let left = layout.content_left() as i32;
        let content = layout.content_width() as i32;
        // The edge of the results' own area: the window's own edge, and
        // the names' edge when the column took the right of the window.
        let edge = if column {
            results_right(&layout)
        } else {
            width
        };

        // The field.
        let field_top = layout.input_top() as i32;
        let field_h = layout.input_height as i32;

        // The time the last answer took, flush right of the query.
        //
        // The number is the headline of the meter: it is the one figure
        // that says whether the search is fast, and it sits where the eye
        // already is — at the end of the line the user is typing on.
        //
        // The query gives up the room of it: a query that runs under the
        // number is a query that hides the number.
        let latency_label = view.latency.last_label();
        select(hdc, fonts.folder);
        let latency_w = match &latency_label {
            Some(label) => text_width(hdc, label),
            None => 0,
        };
        let query_room = content.saturating_sub(if latency_w > 0 {
            latency_w + px(scale, 8)
        } else {
            0
        });

        // The number, before the query: the query is cut to the room that
        // is left, so the two cannot reach each other.
        if let Some(label) = &latency_label {
            draw_run(
                hdc,
                label,
                left + content - latency_w,
                field_top + px(scale, 14),
                colours.latency,
                latency_w,
            );
        }

        select(hdc, fonts.field);
        if view.query.is_empty() {
            let w = draw_run(
                hdc,
                &view.placeholder,
                left,
                field_top + px(scale, 14),
                colours.dim,
                query_room,
            );
            let _ = w;
        } else {
            // The caret goes where the model says it is, and not after the
            // last character: the model moves it with the arrow keys, and
            // a paste lands where it is -- so a caret that is always at
            // the end puts the blinking line away from the text that
            // just arrived.
            let (before, after) = split_at_caret(&view.query, view.caret);
            let w = draw_run(
                hdc,
                before,
                left,
                field_top + px(scale, 14),
                colours.text,
                query_room,
            );
            // The caret: two pixels, two pixels after the text before it.
            // Its height is the text's height and not the field's: the
            // field holds the line under the query too, and a caret as
            // tall as the field is a caret that reaches past the text.
            fill(
                hdc,
                left + w + px(scale, 2),
                field_top + px(scale, 12),
                px(scale, 2),
                px(scale, 21),
                colours.text,
            );
            // The text after the caret starts where the caret ends.
            draw_run(
                hdc,
                after,
                left + w + px(scale, 5),
                field_top + px(scale, 14),
                colours.text,
                query_room.saturating_sub(w + px(scale, 5)),
            );
        }

        // The status line, under the field, and the figures of the meter
        // on its right.
        //
        // The line is the field's own: it ends six units above the
        // field's bottom, and the rows start where the field ends. A
        // line that reaches into the rows is a line the first row's
        // band paints over.
        //
        // The figures share that line because the window has one line
        // under the field, and a window that grew a second one for the
        // meter would be a window that moves under the user's hands.
        // The status is prose and can be cut; the figures are a number
        // with a unit and a chart, and they are the ones that stay.
        let figures = latency_figures(&view.latency, lang.text());
        select(hdc, fonts.folder);
        let figures_w = if figures.is_empty() {
            0
        } else {
            text_width(hdc, &figures)
        };
        let status_room = content.saturating_sub(if figures_w > 0 {
            figures_w + px(scale, 8)
        } else {
            0
        });
        if !view.status.is_empty() {
            draw_run(
                hdc,
                &view.status,
                left,
                field_top + field_h - px(scale, STATUS_HEIGHT),
                colours.dim,
                status_room,
            );
        }
        if !figures.is_empty() {
            draw_run(
                hdc,
                &figures,
                left + content - figures_w,
                field_top + field_h - px(scale, STATUS_HEIGHT),
                colours.latency,
                figures_w,
            );
        }

        // The rows.
        let visible = view.rows.len();
        // The names have the width the results have: the column took the
        // right of the window, and the results gave up the room of it.
        let names = results_width(&layout);
        for (i, row) in view.rows.iter().enumerate().take(visible) {
            let top = layout.row_top(i as u32) as i32;
            let row_h = layout.row_height as i32;
            if i == view.selected {
                fill(hdc, 0, top, edge, row_h, colours.selected);
                // The accent bar on the left: the selection's own edge.
                fill(hdc, 0, top, px(scale, 3), row_h, colours.accent);
            }
            draw_row(
                hdc, fonts, colours, row, left, top, row_h, names, icons, scale,
            );
        }

        // The actions, in the column on the right of the window.
        if column {
            draw_column(hdc, fonts, colours, view, &layout, height, scale, lang);
        }

        // The preview panel, over everything the rows were.
        if let Some(preview) = &view.preview {
            let top = layout.list_top() as i32;
            draw_preview(
                hdc,
                fonts,
                colours,
                preview,
                left,
                top,
                edge,
                height - top,
                &layout,
            );
        }

        // The settings panel, over everything the rows were, like the
        // preview: the window does not grow for a panel the user reads
        // for a moment and closes.
        if view.mode == Mode::Settings && !view.settings.is_empty() {
            let top = layout.list_top() as i32;
            draw_settings(
                hdc,
                fonts,
                colours,
                view,
                left,
                top,
                edge,
                height - top,
                &layout,
                scale,
            );
        }

        // The context is left as it was found.
        select(hdc, stock);
    };
    (width, height)
}

/// The figures of the meter, as the window draws them: the chart of the
/// last few dozen answers, and the percentiles beside it.
///
/// The order is the terminal's: the chart first and the numbers after,
/// because the chart says whether a scan got slower and the numbers say
/// by how much.
///
/// An empty meter gives an empty string: a window that draws a chart of
/// nothing is a window that reports a measurement it never took.
fn latency_figures(latency: &Latency, text: &Text) -> String {
    if latency.is_empty() {
        return String::new();
    }
    let mut out = String::new();
    if !latency.spark.is_empty() {
        out.push_str(&latency.spark);
        out.push_str("  ");
    }
    if let Some(stats) = latency.stats(text) {
        out.push_str(&stats);
    }
    out
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
/// struct that only this function and its caller ever see. The colours
/// are the paint's, handed down from the window that holds the theme.
#[allow(clippy::too_many_arguments)]
fn draw_preview(
    hdc: HDC,
    fonts: &Fonts,
    colours: &Colours,
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
    fill(hdc, 0, top, width, height, colours.backdrop);
    frame(hdc, 0, top, width, height, colours.border);
    select(hdc, fonts.row);
    draw_run(
        hdc,
        &preview.title,
        left,
        top + layout.preview_title_top as i32,
        colours.text,
        width.saturating_sub(left),
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
        draw_run(hdc, &text, left, y, colours.dim, width.saturating_sub(left));
        y += line_height;
    }
}

/// The settings panel: a list of rows, a label and a value on each.
///
/// The picture is the menu's and the preview's, together: the panel
/// covers the results the way the preview does, and the selected row is
/// marked the way the menu marks its own — the band, and the accent bar
/// at its left edge, only while the panel has the keyboard. The label
/// has what is left of the width once the value is measured: a value
/// that did not fit would be cut and not drawn over.
#[allow(clippy::too_many_arguments)]
fn draw_settings(
    hdc: HDC,
    fonts: &Fonts,
    colours: &Colours,
    view: &View,
    left: i32,
    top: i32,
    width: i32,
    height: i32,
    layout: &Layout,
    scale: u32,
) {
    if height <= 0 {
        return;
    }
    fill(hdc, 0, top, width, height, colours.backdrop);
    frame(hdc, 0, top, width, height, colours.border);
    let row = layout.row_height as i32;
    let inner = (width - px(scale, 12) * 2).max(0);
    for (i, (label, value)) in view.settings.iter().enumerate() {
        let y = top + px(scale, PANEL_INSET as i32) + i as i32 * row;
        if y + row > top + height {
            // The panel ran out of room: the rows it could not show are
            // the arrows' to reach, and a panel that drew half a row
            // would be a panel that drew a word cut in its middle.
            break;
        }
        if i == view.settings_selected {
            fill(hdc, 0, y, width, row, colours.selected);
            fill(hdc, 0, y, px(scale, 3), row, colours.accent);
        }
        // The value is measured first, the way the column measures the
        // shortcut: the label has what is left, and neither is drawn
        // over the other.
        select(hdc, fonts.folder);
        let value = shorten(value, 24);
        let value_width = text_width(hdc, &value).min(inner);
        select(hdc, fonts.row);
        draw_run(
            hdc,
            label,
            left,
            y + px(scale, 5),
            colours.dim,
            inner - value_width,
        );
        select(hdc, fonts.folder);
        draw_run(
            hdc,
            &value,
            left + inner - value_width,
            y + px(scale, 6),
            colours.text,
            value_width,
        );
    }
}

/// One row: the icon, the name with its match picked out, and the folder
/// under it.
///
/// The width the row has is the width the results have: the column took
/// the right of the window, and a name that does not fit is cut at the
/// edge of the results and not drawn over the column.
///
/// The geometry is four numbers and three words, and a struct would be a
/// struct that only this function and its caller ever see. The colours
/// are the paint's, handed down from the window that holds the theme.
#[allow(clippy::too_many_arguments)]
fn draw_row(
    hdc: HDC,
    fonts: &Fonts,
    colours: &Colours,
    row: &Row,
    left: i32,
    top: i32,
    row_h: i32,
    width: i32,
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
    let text_width = width.saturating_sub(gutter);
    {
        select(hdc, fonts.row);
        let name = row.name();
        let mut x = text_left;
        for (text, matched) in runs(name, row.matched()) {
            // The run is cut at the room that is left: a name that does
            // not fit ends at the edge of the results.
            let room = text_width.saturating_sub((x - text_left).max(0));
            if room <= 0 {
                break;
            }
            let colour = if matched {
                colours.match_
            } else {
                colours.text
            };
            x += draw_run(hdc, &text, x, top + px(scale, 5), colour, room);
            if x - text_left >= text_width {
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
                colours.dim,
                text_width,
            );
        }
    }
}

/// The height of the line under the field, in the model's units.
///
/// The line is drawn with the small face -- thirteen units of it -- and
/// six units of room under it before the field ends.
const STATUS_HEIGHT: i32 = 19;

/// A rectangle of the picture, in device pixels.
///
/// The drawing is a picture in pixels, and a rectangle is the piece of it
/// that one thing is drawn on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Rect {
    /// The rectangle's left edge.
    left: i32,
    /// The rectangle's top edge.
    top: i32,
    /// The rectangle's width.
    width: i32,
    /// The rectangle's height.
    height: i32,
}

/// The column of actions: where it is and how tall its entries are.
///
/// The column is inside the window: the results gave up the room of it,
/// and the window did not grow.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Column {
    /// The column's own rectangle.
    rect: Rect,
    /// The height of one entry, in device pixels.
    row: i32,
}

impl Column {
    /// The column in this room, with entries of this height.
    ///
    /// The entries are as tall as they can be and no taller: the room
    /// they have is what the results gave up, and a short window shrinks
    /// them. Only a window that cannot hold even the shortest entry has
    /// no column to draw — and the window says so rather than drawing
    /// half of the column.
    fn at(rect: Rect, entries: usize, row: i32, shortest: i32) -> Option<Column> {
        if entries == 0 {
            return None;
        }
        let row = (rect.height / entries as i32).clamp(shortest, row);
        if row * entries as i32 > rect.height {
            // The window has no room for the column.
            return None;
        }
        Some(Column { rect, row })
    }

    /// The top of one entry, in device pixels.
    fn entry_top(&self, index: usize) -> i32 {
        self.rect.top + index as i32 * self.row
    }
}

/// The rectangle of the column, in device pixels.
///
/// The column starts at the top of the results and ends with the
/// content: the window's own margin is not the column's, and the column
/// is flush with the right of the content.
fn column_rect(layout: &Layout, window_height: i32) -> Rect {
    Rect {
        left: layout.side_bar_left() as i32,
        top: layout.list_top() as i32,
        width: layout.side_bar_width() as i32,
        height: (window_height - layout.border as i32 - layout.padding as i32)
            .saturating_sub(layout.list_top() as i32),
    }
}

/// The width the names have, in device pixels.
///
/// The results give up the room of the column: the window does not grow,
/// so the names are what is left of the content.
fn results_width(layout: &Layout) -> i32 {
    layout.list_width() as i32
}

/// The right edge of the results, in device pixels.
///
/// The results end where the column's gap begins: a column that touches
/// the names reads as part of them.
fn results_right(layout: &Layout) -> i32 {
    layout.content_left() as i32 + results_width(layout)
}

/// The column of actions, on the right of the window.
///
/// The column is the actions of the selected result, and it is where the
/// results gave up the room: the window does not grow, so the names have
/// less of it.
///
/// The column is the window's own colour — the panel's alpha is worked
/// out from that colour, and a shade of its own would make the column
/// opaque where the rest of the window is not — and the hairline on its
/// left edge is what says that it is a separate thing.
///
/// The column is always drawn, and it is the **menu** only while it has
/// the keyboard. A mark that is there whether the menu has the keyboard or
/// not is a mark that means nothing: the user pressed `Ctrl+K`, the
/// keyboard moved to the column, and the picture said the same thing it
/// said before. So the mark on the selected entry is there while the menu
/// has the keyboard and not one moment before, and the hairline on the
/// column's left edge is the accent — the colour that says what the
/// keyboard is on — for as long as the menu holds it.
///
/// The labels are the vocabulary's, in the language the window speaks: a
/// menu that says "Open" to a user who reads "Abrir" is a menu the user
/// has to guess.
#[allow(clippy::too_many_arguments)]
fn draw_column(
    hdc: HDC,
    fonts: &Fonts,
    colours: &Colours,
    view: &View,
    layout: &Layout,
    window_height: i32,
    scale: u32,
    lang: Lang,
) {
    let rect = column_rect(layout, window_height);
    let row = px(scale, SIDE_BAR_ENTRY_HEIGHT as i32);
    let Some(column) = Column::at(
        rect,
        view.menu.len(),
        row,
        px(scale, SIDE_BAR_MINIMUM_ENTRY_HEIGHT as i32),
    ) else {
        // The window has no room for the column: the column says so
        // instead of the window saying it over its own results.
        draw_too_short(hdc, fonts, colours, rect, scale, lang);
        return;
    };
    // Whether the menu has the keyboard: it is the difference between a
    // column that is a legend of the keys and a column the user is in.
    let live = view.mode == Mode::Menu;
    // The separator: a hairline on the column's left edge, the window's
    // own border on the left of the names.
    fill(
        hdc,
        rect.left,
        rect.top,
        px(scale, 1),
        rect.height,
        if live { colours.accent } else { colours.border },
    );
    // The room inside the column, once its own margins are taken.
    let inner = (rect.width - px(scale, 12) * 2).max(0);
    for (i, action) in view.menu.iter().enumerate() {
        let y = column.entry_top(i);
        if live && i == view.menu_selected {
            // The selected entry is marked the way the selected row is
            // marked: the band, and the bar on its left. It is marked only
            // while the menu has the keyboard — the mark says which action
            // the arrows would run, and the arrows only run an action
            // while the menu has them.
            fill(hdc, rect.left, y, rect.width, column.row, colours.selected);
            fill(hdc, rect.left, y, px(scale, 3), column.row, colours.accent);
        }
        // The shortcut is measured first: the label has what is left of
        // the column's width, and a label that does not fit is cut and
        // not drawn over the shortcut.
        select(hdc, fonts.folder);
        let shortcut = action.shortcut();
        let shortcut_width = text_width(hdc, shortcut).min(inner);
        select(hdc, fonts.row);
        draw_run(
            hdc,
            action.label(lang),
            rect.left + px(scale, 12),
            y + px(scale, 6),
            colours.text,
            inner - shortcut_width,
        );
        select(hdc, fonts.folder);
        draw_run(
            hdc,
            shortcut,
            rect.left + rect.width - px(scale, 12) - shortcut_width,
            y + px(scale, 7),
            colours.dim,
            shortcut_width,
        );
    }
}

/// What the column says when the window is too short for it.
///
/// The column is the actions of the selected result, and the window it
/// is in can be shorter than the list: the screen is not tall enough for
/// the column, and the column says so, in the language the window
/// speaks, because the model cannot know how tall the window ended up.
///
/// The sentence is the vocabulary's: the column is the window's own
/// drawing, and the words it draws are the words the user reads. It is
/// inside the column and not over the window: the results are the
/// window's own business, and a window that says "too short" over its
/// own results has thrown them away.
fn draw_too_short(hdc: HDC, fonts: &Fonts, colours: &Colours, rect: Rect, scale: u32, lang: Lang) {
    select(hdc, fonts.folder);
    draw_run(
        hdc,
        Text::for_lang(lang).no_room_for_the_actions,
        rect.left + px(scale, 12),
        rect.top + px(scale, 4),
        colours.dim,
        rect.width.saturating_sub(px(scale, 24)),
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
///
/// The text is cut at the width it is given: a run that does not fit is
/// cut with an ellipsis, and not drawn over the thing next to it.
fn draw_run(hdc: HDC, text: &str, x: i32, y: i32, colour: u32, max_width: i32) -> i32 {
    let w: Vec<u16> = text.encode_utf16().collect();
    if w.is_empty() {
        return 0;
    }
    let size = text_extent(hdc, &w);
    unsafe {
        SetTextColor(hdc, colour);
        let mut r = RECT {
            left: x,
            top: y,
            // The width the run has: a run that does not fit in it is
            // cut, and the cut belongs to the run.
            right: x + max_width.max(0),
            bottom: y + size.y,
        };
        DrawTextW(
            hdc,
            w.as_ptr(),
            w.len() as i32,
            &mut r,
            DT_LEFT | DT_SINGLELINE | DT_NOPREFIX | DT_END_ELLIPSIS,
        );
    }
    if max_width > 0 {
        size.x.min(max_width)
    } else {
        size.x
    }
}

/// The size of a run of text, in device pixels.
fn text_extent(hdc: HDC, text: &[u16]) -> POINT {
    let mut size = POINT { x: 0, y: 0 };
    unsafe {
        GetTextExtentPoint32W(hdc, text.as_ptr(), text.len() as i32, &mut size);
    }
    size
}

/// The width of a run of text, in device pixels.
///
/// The width is asked before the text is drawn: the shortcut of an entry
/// sits on the right of the column, and the label has what is left of
/// the column's width.
fn text_width(hdc: HDC, text: &str) -> i32 {
    let w: Vec<u16> = text.encode_utf16().collect();
    if w.is_empty() {
        return 0;
    }
    text_extent(hdc, &w).x
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
    use rgy_launcher::latency::LatencyMeter;

    use super::*;

    /// Draw a view and hand back the surface it was drawn on.
    ///
    /// The picture is of the window the view describes: the same geometry
    /// the window's procedure would have used — and the geometry is the
    /// paint's own (`model_of`), so a test cannot size the surface with a
    /// rule the picture does not follow.
    fn draw(view: &View) -> (Canvas, i32, i32) {
        let model = model_of(view);
        let width = device_pixels(model.width, SCALE_PERCENT) as i32;
        let height = device_pixels(model.height(), SCALE_PERCENT) as i32;
        let Some(canvas) = Canvas::new(width, height) else {
            panic!("the platform gave no surface to draw on");
        };
        let fonts = Fonts::new(SCALE_PERCENT);
        let mut icons = Icons::new();
        let painted = unsafe {
            paint(
                canvas.dc,
                view,
                false,
                &mut icons,
                &fonts,
                &Colours::WARM,
                SCALE_PERCENT,
                Lang::English,
            )
        };
        assert_eq!(painted, (width, height), "the picture is the surface");
        (canvas, width, height)
    }

    /// The green pixels of the picture: the meter is green and nothing
    /// else in the window is.
    ///
    /// The test asks for the hue and not for the exact colour: GDI blends
    /// the edges of a glyph with what is under it, so the green of a
    /// letter is never exactly the green of the brush.
    fn greens(canvas: &Canvas, width: i32, height: i32) -> Vec<(i32, i32)> {
        let mut out = Vec::new();
        for y in 0..height {
            for x in 0..width {
                let (r, g, b) = canvas.pixel(x, y);
                if g as i32 - r as i32 > 30 && g as i32 - b as i32 > 30 {
                    out.push((x, y));
                }
            }
        }
        out
    }

    /// A view with one answer's worth of time in it.
    fn view_with_latency() -> View {
        let mut meter = LatencyMeter::new();
        meter.record(8_100);
        View {
            query: "rela".to_string(),
            latency: meter.view(24),
            ..View::default()
        }
    }

    /// How many pixels of one exact colour the picture has.
    ///
    /// A fill is exact — GDI blends the glyphs, but a rectangle of one
    /// colour is that colour everywhere it was not drawn over — and the
    /// panel's own marks are fills.
    fn count(canvas: &Canvas, width: i32, height: i32, colour: (u8, u8, u8)) -> usize {
        let mut found = 0;
        for y in 0..height {
            for x in 0..width {
                if canvas.pixel(x, y) == colour {
                    found += 1;
                }
            }
        }
        found
    }

    /// A width the user chose reaches the picture, and not just the window.
    ///
    /// The host sizes the window from `App::layout().width`, and the view
    /// carries that width. `model_of` used to start from the geometry's
    /// default (600 units) and never applied it: a 760-unit window was
    /// drawn 600 units wide, the query, row bands and actions column all
    /// positioned for 600, and the unpainted right strip turned into a
    /// solid black bar in `Layer::compose`.
    #[test]
    fn a_width_the_user_chose_reaches_the_picture() {
        let width = 760;
        let mut rows = Vec::new();
        for i in 0..6 {
            rows.push(Row::new(format!("file-{i}.txt"), 0));
        }
        let view = View {
            width,
            query: "rela".to_string(),
            rows,
            // Four actions make the column appear, so its position is a
            // fact about the width and not only about the rows.
            menu: alloc::vec![
                Action::Open,
                Action::OpenFolder,
                Action::CopyPath,
                Action::Preview,
            ],
            ..View::default()
        };

        let model = model_of(&view);
        assert_eq!(model.width, width, "the model carries the view's width");
        assert!(
            model.side_bar != 0,
            "the actions column has room and is drawn"
        );
        // The column and the content are positioned for the wider window,
        // not for the default 600: their right edge moved with the width.
        assert_eq!(
            model.content_width(),
            width - (model.border + model.padding) * 2,
            "the content spans the wider window"
        );
        assert_eq!(
            model.side_bar_right(),
            width - (model.border + model.padding),
            "the column sits at the right of the wider window"
        );

        // The window's own device size, at this screen's scale.
        let surface = (
            device_pixels(model.width, SCALE_PERCENT) as i32,
            device_pixels(model.height(), SCALE_PERCENT) as i32,
        );
        let Some(canvas) = Canvas::new(surface.0, surface.1) else {
            panic!("the platform gave no surface to draw on");
        };
        let fonts = Fonts::new(SCALE_PERCENT);
        let mut icons = Icons::new();
        let painted = unsafe {
            paint(
                canvas.dc,
                &view,
                false,
                &mut icons,
                &fonts,
                &Colours::WARM,
                SCALE_PERCENT,
                Lang::English,
            )
        };
        assert_eq!(painted, surface, "the picture is as wide as the window");

        // The strip a 600-unit picture would have left is painted: an
        // unpainted pixel is (0, 0, 0), and the compositor gives it full
        // alpha — a solid black bar at the right of the window.
        assert_eq!(
            canvas.pixel(surface.0 - 16, 4),
            in_pixels(Colours::WARM.backdrop),
            "the right of the wider window is painted, not left bare"
        );
    }

    /// The settings panel covers the rows and marks its selected row.
    ///
    /// The panel is over the results, like the preview: the names that
    /// were under it are not on the screen, and the row the arrows are
    /// on carries the band. Both are the picture's own facts, and the
    /// picture is the only place they can be checked.
    #[test]
    fn the_settings_panel_covers_the_rows_and_marks_its_own() {
        let mut view = view_with_menu(Mode::Typing);
        view.settings = vec![
            ("theme".to_string(), "warm".to_string()),
            ("width".to_string(), "600".to_string()),
        ];
        let (plain, width, height) = draw(&view);
        // The selected band of a result row, without the panel. The
        // canvas answers in the order the platform stores a colour:
        // blue first.
        let band = (0x2E, 0x28, 0x26);
        let with_rows = count(&plain, width, height, band);

        view.mode = Mode::Settings;
        view.settings_selected = 1;
        let (panelled, width, height) = draw(&view);
        // The panel's own selected row carries the band too — the same
        // mark the menu makes — and the names under the panel are gone:
        // the picture with the panel has strictly more of the band (the
        // panel's row) and the rows' own glyphs are covered by it.
        let with_panel = count(&panelled, width, height, band);
        assert!(
            with_panel > with_rows,
            "the panel's selected row carries the band: {with_panel} over {with_rows}"
        );
        // The names were bright text on a dark panel; under the settings
        // panel the list area is the panel's own near-black, and the
        // only bright things left are the panel's own words.
        let bright = |canvas: &Canvas| -> usize {
            let mut found = 0;
            for y in 0..height {
                for x in 0..width {
                    let (r, g, b) = canvas.pixel(x, y);
                    if r > 200 && g > 200 && b > 200 {
                        found += 1;
                    }
                }
            }
            found
        };
        assert!(
            bright(&panelled) < bright(&plain),
            "the panel covers the names that were under it"
        );
    }

    /// The settings panel makes its own room in a picture with no rows.
    ///
    /// The live defect this pins, measured on 2026-11-04: `>settings` is a
    /// command and a command has no results, so the picture's geometry
    /// gave the panel a list area of zero — the window grew to the
    /// panel's height and the panel was drawn nowhere, while the arrows
    /// adjusted settings the user could not see. The panel is a list of
    /// its own, like the preview, and the picture says so.
    #[test]
    fn the_settings_panel_makes_its_own_room_in_a_picture_with_no_rows() {
        let view = View {
            query: ">settings".to_string(),
            mode: Mode::Settings,
            settings: (0..12)
                .map(|i| (alloc::format!("row{i}"), "value".to_string()))
                .collect(),
            settings_selected: 0,
            ..View::default()
        };
        let (canvas, width, height) = draw(&view);
        // The window is the field, the line under it, and twelve rows of
        // the panel: the row worth of room is what the panel brings.
        let one_row = Layout::default().row_height;
        let field = Layout::default().input_height + Layout::default().padding * 2;
        assert!(
            height >= (field + 12 * one_row) as i32,
            "the picture is {height} pixels tall for a twelve-row panel"
        );
        // And the panel's selected row is really on it: the band the menu
        // uses, in the panel's own area.
        let band = (0x2E, 0x28, 0x26);
        assert!(
            count(&canvas, width, height, band) > 0,
            "the panel's selected row carries its band"
        );
    }

    /// The settings picture is the same width with and without stale
    /// results.
    ///
    /// The actions column used to be added in `Mode::Settings`, and with
    /// four results left over from the last answer it took the side
    /// bar's width away from the settings panel. The picture gates the
    /// column on the modes where the actions are reachable, exactly as
    /// `App::layout` does, so the panel owns the whole content width
    /// either way.
    #[test]
    fn the_settings_picture_is_not_squeezed_by_stale_results() {
        let mut view = View {
            query: ">settings".to_string(),
            mode: Mode::Settings,
            settings: (0..12)
                .map(|i| (alloc::format!("row{i}"), "value".to_string()))
                .collect(),
            menu: vec![
                Action::Open,
                Action::OpenFolder,
                Action::CopyPath,
                Action::Preview,
            ],
            ..View::default()
        };
        let without_rows = model_of(&view);
        view.rows = vec![
            Row::new("C:/a/one.txt".to_string(), 0),
            Row::new("C:/a/two.txt".to_string(), 1),
            Row::new("C:/a/three.txt".to_string(), 2),
            Row::new("C:/a/four.txt".to_string(), 3),
        ];
        let with_rows = model_of(&view);

        assert_eq!(
            without_rows.side_bar_width(),
            0,
            "the panel, not the actions, has the keyboard"
        );
        assert_eq!(
            with_rows.side_bar_width(),
            0,
            "stale results do not bring the actions column back"
        );
        assert_eq!(
            with_rows.list_width(),
            without_rows.list_width(),
            "the panel owns the whole content width either way"
        );
    }

    /// A view with a column of actions, in the mode it was given.
    ///
    /// The column is the same in both modes: the actions of the selected
    /// result, and the key that does each one. What changes is whether the
    /// keyboard is on it.
    ///
    /// The rows are four because the column needs the room: a window with
    /// one result is shorter than four actions, and a column that does not
    /// fit is not drawn at all.
    fn view_with_menu(mode: Mode) -> View {
        View {
            query: "toml".to_string(),
            rows: vec![
                Row::new("C:/a/Cargo.toml".to_string(), 0),
                Row::new("C:/a/Cargo.lock".to_string(), 1),
                Row::new("C:/a/crates.toml".to_string(), 2),
                Row::new("C:/a/pyproject.toml".to_string(), 3),
            ],
            menu: vec![
                Action::Open,
                Action::OpenFolder,
                Action::CopyPath,
                Action::Preview,
            ],
            menu_selected: 0,
            mode,
            ..View::default()
        }
    }

    /// Where the column is, and how tall its entries are.
    ///
    /// The geometry is the one the picture used: the side bar is in it,
    /// because the column's own edge is where the side bar's left edge
    /// is, and a column measured without it would be somewhere else.
    fn column_geometry(view: &View, height: i32) -> (Rect, i32) {
        let model = Layout::with_rows(view.rows.len() as u32).with_side_bar(SIDE_BAR_WIDTH);
        let layout = model.scaled(SCALE_PERCENT);
        let rect = column_rect(&layout, height);
        let row = px(SCALE_PERCENT, SIDE_BAR_ENTRY_HEIGHT as i32);
        let column = Column::at(
            rect,
            view.menu.len(),
            row,
            px(SCALE_PERCENT, SIDE_BAR_MINIMUM_ENTRY_HEIGHT as i32),
        )
        .expect("the column is there");
        (rect, column.row)
    }

    /// The warm theme is the palette the window was drawn in.
    ///
    /// `Colours::of` maps the theme's roles onto the paint's fields one
    /// for one, and the theme's first duty is to answer the constants it
    /// replaced: a theme that moved a colour nobody asked about is a
    /// theme that redraws the launcher under its users. The fields are
    /// compared one by one so a failure says which role moved.
    #[test]
    fn the_warm_theme_is_the_palette_the_window_was_drawn_in() {
        let of = Colours::of(&Theme::WARM);
        assert_eq!(of.text, Colours::WARM.text, "text");
        assert_eq!(of.dim, Colours::WARM.dim, "dim");
        assert_eq!(of.match_, Colours::WARM.match_, "matched");
        assert_eq!(of.selected, Colours::WARM.selected, "selected");
        assert_eq!(of.accent, Colours::WARM.accent, "accent");
        assert_eq!(of.latency, Colours::WARM.latency, "latency");
        assert_eq!(of.backdrop, Colours::WARM.backdrop, "backdrop");
        assert_eq!(of.border, Colours::WARM.border, "border");
        assert_eq!(of.background_blue, Colours::WARM.background_blue, "blue");
        assert_eq!(of.background_green, Colours::WARM.background_green, "green");
        assert_eq!(of.background_red, Colours::WARM.background_red, "red");
    }

    /// The menu says that it has the keyboard.
    ///
    /// The column of actions is drawn whether the menu is open or not — it
    /// is the actions of the selected result and the key that does each
    /// one — and a mark that is there either way says nothing: the user
    /// pressed `Ctrl+K`, the keyboard moved to the column, and the picture
    /// said the same thing it said before.
    ///
    /// So the selected action is marked while the menu has the keyboard and
    /// not one moment before, and the test draws the picture and looks for
    /// the mark: a rule about the picture is only checkable by drawing it.
    #[test]
    fn the_menu_is_marked_only_while_it_has_the_keyboard() {
        let closed = view_with_menu(Mode::Typing);
        let (canvas, _width, height) = draw(&closed);
        let (rect, _row) = column_geometry(&closed, height);
        assert_ne!(
            canvas.pixel(rect.left + 1, rect.top + 2),
            in_pixels(Colours::WARM.accent),
            "the menu is closed and the action is still marked"
        );

        let open = view_with_menu(Mode::Menu);
        let (canvas, _width, height) = draw(&open);
        let (rect, _row) = column_geometry(&open, height);
        assert_eq!(
            canvas.pixel(rect.left + 1, rect.top + 2),
            in_pixels(Colours::WARM.accent),
            "the action the arrows would run is not marked"
        );
    }

    /// The column's edge is the accent while the menu has the keyboard.
    ///
    /// The hairline is what says that the column is a separate thing, and
    /// it is the accent — the colour that says what the keyboard is on —
    /// for as long as the menu holds it. The picture says where the
    /// keyboard is before the user presses a key and finds out.
    #[test]
    fn the_column_s_edge_is_the_accent_while_the_menu_has_the_keyboard() {
        let closed = view_with_menu(Mode::Typing);
        let (canvas, _width, height) = draw(&closed);
        let (rect, _row) = column_geometry(&closed, height);
        assert_eq!(
            canvas.pixel(rect.left, rect.top + 40),
            in_pixels(Colours::WARM.border),
            "the edge is not the border's colour"
        );

        let open = view_with_menu(Mode::Menu);
        let (canvas, _width, height) = draw(&open);
        let (rect, _row) = column_geometry(&open, height);
        assert_eq!(
            canvas.pixel(rect.left, rect.top + 40),
            in_pixels(Colours::WARM.accent),
            "the edge does not say that the menu has the keyboard"
        );
    }

    /// The window shows the time the answers take.
    ///
    /// The number is green and flush right of the query: the terminal puts
    /// the same figure in the same place and in the same colour, and a
    /// figure that is in another place in one of the two faces is a
    /// figure the user has to look for twice.
    ///
    /// The test draws the picture and looks for the green: a rule about
    /// the picture is only checkable by drawing it.
    #[test]
    fn the_time_the_answers_take_is_in_the_picture() {
        let view = view_with_latency();
        let (canvas, width, height) = draw(&view);
        let greens = greens(&canvas, width, height);
        assert!(!greens.is_empty(), "the time is not in the picture");

        // The number is on the field's own line, and it is at the right
        // of it: the field's line ends before the row's band starts.
        let model = Layout::with_rows(view.rows.len() as u32);
        let field_top = model.scaled(SCALE_PERCENT).input_top() as i32;
        let on_the_field: Vec<i32> = greens
            .iter()
            .filter(|(_, y)| *y >= field_top && *y < field_top + 36)
            .map(|(x, _)| *x)
            .collect();
        assert!(
            !on_the_field.is_empty(),
            "the number is not on the line of the query"
        );
        let leftmost = on_the_field.iter().min().copied().unwrap_or(0);
        assert!(
            leftmost > width / 2,
            "the number is not flush right: {leftmost} of {width}"
        );
    }

    /// A window with no answer has no time to show.
    ///
    /// A picture that says "0 ms" before the first answer is a picture
    /// that reports a measurement it never took.
    #[test]
    fn a_window_without_an_answer_shows_no_time() {
        let view = View {
            query: "rela".to_string(),
            ..View::default()
        };
        let (canvas, width, height) = draw(&view);
        let greens = greens(&canvas, width, height);
        assert!(
            greens.is_empty(),
            "the picture has a figure it never took: {greens:?}"
        );
    }

    /// The line under the field is the field's own.
    ///
    /// The line says which question the rows answer, and the first row's
    /// band is painted over the bottom of the window: a line that the
    /// band hides is a line the user cannot read.
    #[test]
    fn the_line_under_the_field_is_above_the_first_row() {
        let view = View {
            query: "rela".to_string(),
            status: "6 resultados".to_string(),
            ..View::default()
        };
        // The picture is of the window the view describes: the same
        // geometry the window's procedure would have used.
        let model = Layout::with_rows(view.rows.len() as u32);
        let layout = model.scaled(SCALE_PERCENT);
        let width = device_pixels(model.width, SCALE_PERCENT) as i32;
        let height = device_pixels(model.height(), SCALE_PERCENT) as i32;
        let Some(canvas) = Canvas::new(width, height) else {
            panic!("the platform gave no surface to draw on");
        };
        let fonts = Fonts::new(SCALE_PERCENT);
        let mut icons = Icons::new();
        let _ = unsafe {
            paint(
                canvas.dc,
                &view,
                false,
                &mut icons,
                &fonts,
                &Colours::WARM,
                SCALE_PERCENT,
                Lang::English,
            )
        };
        // The lowest thing on the picture is the line under the field,
        // and it is above the rows: the first row's band is painted over
        // the bottom of the window, and a line under the band is a line
        // nobody reads. The border is the window's own and is not a row.
        let mut lowest = 0;
        for y in 0..height - 2 {
            for x in 1..width - 1 {
                if canvas.pixel(x, y) != in_pixels(Colours::WARM.backdrop) {
                    lowest = y;
                }
            }
        }
        assert!(
            lowest < layout.list_top() as i32,
            "the line under the field is under the first row: {lowest}"
        );
    }

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

    /// The column is where the layout says it is.
    ///
    /// The column is inside the window: it starts at the top of the
    /// results and ends with the content, and the window's own margin is
    /// not the column's.
    #[test]
    fn the_column_is_where_the_layout_says() {
        let model = Layout::with_rows(4).with_side_bar(SIDE_BAR_WIDTH);
        let layout = model.scaled(SCALE_PERCENT);
        let height = device_pixels(model.height(), SCALE_PERCENT) as i32;
        let rect = column_rect(&layout, height);
        assert_eq!(rect.left, layout.side_bar_left() as i32);
        assert_eq!(rect.width, layout.side_bar_width() as i32);
        assert_eq!(
            rect.left + rect.width,
            layout.side_bar_right() as i32,
            "the column is flush with the right margin"
        );
        assert_eq!(
            rect.top,
            layout.list_top() as i32,
            "the column starts at the top of the results"
        );
    }

    /// The results give up the room of the column.
    ///
    /// The window does not grow: the names are what is left of the
    /// content, and the gap is between the names and the column.
    #[test]
    fn the_results_give_up_the_room_of_the_column() {
        let model = Layout::with_rows(4).with_side_bar(SIDE_BAR_WIDTH);
        let layout = model.scaled(SCALE_PERCENT);
        assert_eq!(
            results_width(&layout),
            layout.content_width() as i32 - SIDE_BAR_WIDTH as i32 - layout.side_bar_gap as i32,
            "the names have what is left of the content"
        );
        assert_eq!(
            results_right(&layout) + layout.side_bar_gap as i32,
            layout.side_bar_left() as i32,
            "the gap is between the names and the column"
        );
    }

    /// Without the actions there is no column, and the results have the
    /// whole of the content.
    #[test]
    fn a_window_without_the_actions_has_the_whole_width() {
        let layout = Layout::with_rows(4).scaled(SCALE_PERCENT);
        assert_eq!(results_width(&layout), layout.content_width() as i32);
    }

    /// The column opens with its first entry.
    ///
    /// The entries are stacked from the top of the results, and the first
    /// one is at the top: an entry that starts lower has a band of
    /// nothing above it, and it reads as a fault in the drawing.
    #[test]
    fn the_column_opens_with_its_first_entry() {
        let rect = Rect {
            left: 395,
            top: 61,
            width: 190,
            height: 120,
        };
        let column = Column::at(rect, 4, 30, 24).expect("the column fits");
        assert_eq!(column.entry_top(0), rect.top, "the first entry is the top");
        assert_eq!(column.entry_top(3), rect.top + 3 * 30);
        assert_eq!(column.row, 30, "the entries are as tall as they can be");
    }

    /// A window with one result is shorter than a column of four entries,
    /// and the entries shrink to fit it.
    #[test]
    fn the_column_shrinks_to_fit_a_short_window() {
        let rect = Rect {
            left: 395,
            top: 61,
            width: 190,
            height: 110,
        };
        let column = Column::at(rect, 4, 30, 24).expect("the column still fits");
        assert_eq!(column.row, 27);
        assert!(
            column.entry_top(3) + column.row <= rect.top + rect.height,
            "the last entry ends inside the column"
        );
    }

    /// A window with no room for the column has no column: the window
    /// says so instead of drawing half of the column.
    #[test]
    fn a_window_with_no_room_for_the_column_has_no_column() {
        let rect = Rect {
            left: 395,
            top: 61,
            width: 190,
            height: 80,
        };
        assert_eq!(Column::at(rect, 4, 30, 24), None);
    }

    /// The column is drawn where the layout says it is.
    ///
    /// The picture is what the user sees: a column that is drawn over
    /// the names, or one that starts lower than the results, is a
    /// picture the layout does not describe.
    ///
    /// The column is drawn with the menu open, because the mark of the
    /// selected entry is what says where the column's first entry is:
    /// without the menu the column is a legend, and a legend's first
    /// letter is a letter and not the top of an entry.
    #[test]
    fn the_column_is_drawn_where_the_layout_says() {
        let model = Layout::with_rows(4).with_side_bar(SIDE_BAR_WIDTH);
        let layout = model.scaled(SCALE_PERCENT);
        let width = device_pixels(model.width, SCALE_PERCENT) as i32;
        let height = device_pixels(model.height(), SCALE_PERCENT) as i32;
        let Some(canvas) = Canvas::new(width, height) else {
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
            mode: Mode::Menu,
            ..View::default()
        };
        // The surface is the window's own colour, so the column's edge
        // is the first thing on it that is not.
        fill(canvas.dc, 0, 0, width, height, Colours::WARM.backdrop);
        draw_column(
            canvas.dc,
            &fonts,
            &Colours::WARM,
            &view,
            &layout,
            height,
            SCALE_PERCENT,
            Lang::English,
        );
        // The separator is on the column's own edge, and it opens where
        // the list opens.
        let edge = layout.side_bar_left() as i32;
        let top = (0..height)
            .find(|&y| canvas.pixel(edge, y) != in_pixels(Colours::WARM.backdrop))
            .expect("the column drew its edge");
        assert_eq!(
            top,
            layout.list_top() as i32,
            "the column opens with the list"
        );
        let x = layout.side_bar_left() as i32 + 10;
        assert_eq!(
            canvas.pixel(x, top),
            in_pixels(Colours::WARM.selected),
            "and the first entry is the selected one"
        );
        unsafe {
            // The font that is still selected cannot be deleted: the
            // stock one goes back in before the window's fonts go.
            let stock = GetCurrentObject(canvas.dc, OBJ_FONT);
            SelectObject(canvas.dc, stock);
        }
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
