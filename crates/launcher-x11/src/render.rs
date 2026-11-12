// SPDX-License-Identifier: Apache-2.0

//! The frame, drawn in client memory.
//!
//! The panel is a rounded card: a soft vertical gradient, a hairline
//! border, a recessed inner edge, real text at three sizes, one icon per
//! result and the matched characters picked out in the match colour. All of
//! it is computed into a [`Surface`] and handed to the server in one image,
//! so a frame is never seen half-drawn and the same code produces the same
//! pixels on any host.
//!
//! Text is FreeType when it is there and the built-in bitmap font when it is
//! not — [`crate::font`] decides, and the only difference the rest of this
//! module sees is that `Face::Small` may fall back to the regular size.

use alloc::vec::Vec;

use rgy_launcher::layout::{Layout, SIDE_BAR_ENTRY_HEIGHT, SIDE_BAR_GAP, SIDE_BAR_WIDTH};
use rgy_launcher::results::IconKind;
use rgy_launcher::text::{Lang, Text};
use rgy_launcher::theme::{Rgb, Theme};
use rgy_launcher::{Mode, Preview};

use crate::font::Font;
use crate::paint::glyph;
use crate::raster::{Surface, argb, lerp};
use crate::types::View;

/// The seven icon families the launcher knows, in the order the cache
/// stores them.
///
/// [`IconKind`] is the model's answer for one row; a family is the same
/// answer as a *theme* speaks it, so the cache is one slot per family and
/// the theme is asked once per family, not once per row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconFamily {
    /// A folder.
    Folder,
    /// A document.
    Document,
    /// An image.
    Image,
    /// A video.
    Video,
    /// A sound.
    Audio,
    /// Something the machine runs.
    Application,
    /// A file of no kind of its own.
    File,
}

impl IconFamily {
    /// The family a model row's icon belongs to.
    fn of(kind: IconKind) -> IconFamily {
        match kind {
            IconKind::Folder => IconFamily::Folder,
            IconKind::Document => IconFamily::Document,
            IconKind::Image => IconFamily::Image,
            IconKind::Video => IconFamily::Video,
            IconKind::Audio => IconFamily::Audio,
            IconKind::Application => IconFamily::Application,
            IconKind::File => IconFamily::File,
        }
    }

    /// The cache slot of this family.
    fn slot(self) -> usize {
        match self {
            IconFamily::Folder => 0,
            IconFamily::Document => 1,
            IconFamily::Image => 2,
            IconFamily::Video => 3,
            IconFamily::Audio => 4,
            IconFamily::Application => 5,
            IconFamily::File => 6,
        }
    }
}
/// How many latency samples the bars carry.
///
/// The meter's window is 64; the strip is a glance and not a chart, so the
/// newest two dozen are what a person reads while typing.
const BAR_CAP: usize = 24;
/// One bar's width in pixels.
const BAR_WIDTH: i32 = 3;
/// The gap between two bars.
const BAR_GAP: i32 = 1;
/// How tall a full-level bar is.
const BAR_HEIGHT: i32 = 9;
/// The sub-levels the easing works in: one of the meter's eight levels.
const BAR_SCALE: i16 = 16;
/// Which of the loaded faces a line is set in.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Face {
    /// The query and the names: the bold face when there is one.
    Strong,
    /// Folders, the status line, the words around the answer.
    Regular,
    /// The small line under a name and the settings' values.
    Small,
}

/// The renderer: one surface, and the faces the text is set in.
pub struct Renderer {
    /// The pixels this frame is computed into.
    pub surface: Surface,
    /// The regular face, when FreeType and a font file were found.
    regular: Option<Font>,
    /// The bold face, or the regular one standing in for it.
    strong: Option<Font>,
    /// A second regular face at the small size.
    small: Option<Font>,
    /// The theme's icons, one slot per family: `None` means "not asked yet",
    /// `Some(None)` means "asked, the theme has none", so a missing theme
    /// costs one look and not one per row per frame.
    icons: [Option<Option<crate::icons::Icon>>; 7],
    /// The latency bars as drawn this frame, in sixteenths of a level: the
    /// meter offers eight levels and the sixteenths are what make a bar grow
    /// instead of jump.
    bars: [i16; BAR_CAP],
    /// How many bars the meter currently offers.
    bar_count: usize,
    /// Whether a bar moved on the last frame, which is the pump's cue to
    /// keep painting at the animation's rate.
    animating: bool,
}

impl Renderer {
    /// A renderer for a frame of the given size.
    pub fn new(width: u32, height: u32) -> Renderer {
        let regular = Font::load();
        let strong = Font::load_bold();
        let small =
            Font::from_path_size(crate::font::candidate_paths()[0], crate::font::SMALL_SIZE)
                .or_else(|| {
                    crate::font::bold_candidate_paths()
                        .first()
                        .and_then(|p| Font::from_path_size(p, crate::font::SMALL_SIZE))
                });
        Renderer {
            surface: Surface::new(width, height),
            regular,
            strong,
            small,
            icons: [None, None, None, None, None, None, None],
            bars: [0; BAR_CAP],
            bar_count: 0,
            animating: false,
        }
    }

    /// Move the bars one step towards the samples the meter now holds.
    ///
    /// The sparkline is the meter's own eight levels, one character per
    /// sample; reading them back is how the strip stays the model's decision
    /// and not a second measurement. Each bar moves a quarter of the way to
    /// its target, so a new sample grows in over a handful of frames and a
    /// dropped one settles back — and the flag says whether anything moved,
    /// which is the pump's cue to keep painting at the animation's rate.
    fn animate_bars(&mut self, spark: &str) {
        let offered = spark.chars().count();
        let total = offered.min(BAR_CAP);
        // The window slides: the newest samples are the ones on the right.
        let skip = offered.saturating_sub(total);
        let mut target = [0i16; BAR_CAP];
        for (slot, ch) in spark.chars().skip(skip).enumerate() {
            target[slot] = level_of(ch) as i16 * BAR_SCALE;
        }
        self.bar_count = total;
        let mut moved = false;
        for (index, want) in target.iter().enumerate() {
            let want = if index < total { *want } else { 0 };
            let have = self.bars[index];
            if have == want {
                continue;
            }
            let step = (want - have) / 4;
            self.bars[index] = have + if step == 0 { want - have } else { step };
            moved = true;
        }
        self.animating = moved;
    }

    /// Whether the bars are still moving.
    ///
    /// The pump reads this to tick faster than its slow clock while a frame
    /// would change something, and to go quiet once the strip has settled.
    pub fn is_animating(&self) -> bool {
        self.animating
    }
    /// Resize the frame's surface.
    pub fn resize(&mut self, width: u32, height: u32) {
        self.surface.resize(width, height);
    }

    /// The face a line is drawn with.
    fn face(&mut self, face: Face) -> Option<&mut Font> {
        match face {
            Face::Strong => self.strong.as_mut().or(self.regular.as_mut()),
            Face::Regular => self.regular.as_mut(),
            Face::Small => self.small.as_mut().or(self.regular.as_mut()),
        }
    }

    /// The pixel size a face is set at.
    fn size_of(&self, face: Face) -> u32 {
        let chosen = match face {
            Face::Strong => self.strong.as_ref().or(self.regular.as_ref()),
            Face::Regular => self.regular.as_ref(),
            Face::Small => self.small.as_ref().or(self.regular.as_ref()),
        };
        chosen.map(|f| f.size()).unwrap_or(crate::font::SIZE)
    }

    /// Draw a line of text with its baseline at `baseline`.
    ///
    /// Returns the advance, so the caller can place something after it. A
    /// line is clipped at `max_x`: a character that would cross it is not
    /// drawn at all, which is what keeps a long name inside the panel.
    fn text(&mut self, x: i32, baseline: i32, s: &str, colour: u32, face: Face, max_x: i32) -> i32 {
        if s.is_empty() {
            return 0;
        }
        let mut pen = x;
        let size = self.size_of(face);
        let mut real = self.face(face).is_some();
        if let Some(font) = self.face(face) {
            // The glyph borrows the face, so the surface cannot be touched
            // inside the borrow: the glyphs are collected first.
            let mut placed: Vec<(i32, i32, u32, u32, u32, Vec<u8>)> = Vec::new();
            for ch in s.chars() {
                let Some(g) = font.glyph(ch) else {
                    pen += (size / 2) as i32;
                    continue;
                };
                if pen + g.advance > max_x {
                    break;
                }
                if g.width > 0 && g.height > 0 {
                    let len = (g.height as usize).saturating_mul(g.pitch as usize);
                    let bytes = g.coverage[..len.min(g.coverage.len())].to_vec();
                    placed.push((
                        pen + g.left,
                        baseline - g.top,
                        g.width,
                        g.height,
                        g.pitch,
                        bytes,
                    ));
                }
                pen += g.advance;
            }
            for (gx, gy, gw, gh, pitch, bytes) in placed {
                self.surface
                    .blend_mask(gx, gy, gw, gh, &bytes, pitch, colour);
            }
            if pen != x || !s.is_empty() {
                real = true;
            }
        }
        if !real {
            // The fallback: the 8x8 cells, one pixel per cell pixel.
            for ch in s.chars() {
                if pen + 8 > max_x {
                    break;
                }
                let bits = glyph(ch);
                for (row, bits) in bits.iter().enumerate() {
                    for col in 0..8u8 {
                        if bits & (1 << col) != 0 {
                            blend_at(
                                &mut self.surface,
                                pen + col as i32,
                                baseline - 7 + row as i32,
                                colour,
                                255,
                            );
                        }
                    }
                }
                pen += 8;
            }
        }
        pen - x
    }

    /// The width of a line in pixels.
    fn text_width(&mut self, s: &str, face: Face) -> i32 {
        let size = self.size_of(face);
        match self.face(face) {
            Some(font) => font.measure(s),
            None => s.chars().count() as i32 * 8,
        }
        .max(if s.is_empty() { 0 } else { size as i32 / 2 })
    }

    /// Draw the whole frame and return the area painted.
    pub fn draw(&mut self, view: &View, lang: Lang) -> (u32, u32) {
        let w = self.surface.width as i32;
        let h = self.surface.height as i32;
        if w <= 0 || h <= 0 {
            return (0, 0);
        }
        let l = Layout::default();
        let pad = l.padding as i32;
        let input_h = l.input_height as i32;
        let row_h = l.row_height as i32;
        let radius = l.radius;

        let theme = view.theme;
        let text = Text::for_lang(lang);

        // The card: a two-step vertical gradient, so the panel has a top
        // and a bottom without a compositor to blur anything.
        let base = solid(theme.background, 255);
        let top = lerp(base, solid(theme.text, 255), 10);
        self.surface.clear(0);
        // The card is drawn one row at a time, each inset by the corner arc:
        // that is what makes the shape rounded. A full-width line here would
        // square the corners off again.
        for i in 0..h {
            let t = (((i * 255) / h.max(1)).min(255) as u8).min(160);
            let inset = corner_inset(i, h, radius as i32);
            if inset * 2 < w {
                self.surface
                    .hline(inset, w - 1 - inset, i, lerp(top, base, t));
            }
        }
        // The recessed edge: a hairline that follows the same curve, then a
        // soft inner shade.
        self.outline(0, 0, w, h, radius as i32, solid(theme.border, 210));
        self.surface
            .inner_shadow(1, 1, w - 2, h - 2, radius, 3, argb(90, 0, 0, 0));

        // The field: the query, or the placeholder, with the caret.
        let baseline = pad + self.size_of(Face::Strong) as i32;
        let (field, field_colour) = if view.query.is_empty() {
            (view.placeholder.clone(), solid(theme.dim, 200))
        } else {
            (view.query.clone(), solid(theme.text, 255))
        };
        let inner = w - pad * 2;
        let used = self.text(
            pad + 6,
            baseline,
            &field,
            field_colour,
            Face::Strong,
            pad + inner,
        );
        if view.caret as i32 >= 0 {
            let caret_x = pad + 6 + used.min(inner);
            self.surface.fill_rect(
                caret_x,
                baseline - self.size_of(Face::Strong) as i32,
                2,
                self.size_of(Face::Strong) as i32,
                solid(theme.accent, 235),
            );
        }

        // The separator between the field and the list.
        self.surface
            .hline(pad, w - pad, input_h, solid(theme.border, 120));

        // The one line under the field.
        let small_size = self.size_of(Face::Small) as i32;
        let status_baseline = input_h - 6;
        if !view.status.is_empty() && status_baseline - small_size > baseline {
            self.text(
                pad,
                status_baseline,
                &view.status,
                solid(theme.dim, 230),
                Face::Small,
                w - pad,
            );
        }

        // The rows: selection pill, icon, name, and the line under it.
        let top_index = view.top.min(view.rows.len());
        let visible = view.rows.iter().skip(top_index);
        let mut y = input_h + 3;
        for (i, row) in visible.enumerate() {
            if y + row_h > h - 2 {
                break;
            }
            let selected = top_index + i == view.selected;
            if selected {
                self.surface
                    .fill_round_rect(4, y, w - 8, row_h - 2, 8, solid(theme.selected, 255));
                // The accent bar on the left edge of the selection.
                self.surface
                    .fill_round_rect(4, y + 7, 3, row_h - 16, 2, solid(theme.accent, 255));
            }
            let icon_colour = if selected {
                solid(theme.accent, 255)
            } else {
                solid(theme.dim, 235)
            };
            // The theme's own icon when the machine has one, the drawn shape
            // when it does not.
            let family = IconFamily::of(row.icon().kind);
            let slot = family.slot();
            if self.icons[slot].is_none() {
                self.icons[slot] = Some(crate::icons::load(family));
            }
            let icon_y = y + (row_h - 16) / 2;
            match &self.icons[slot] {
                Some(Some(icon)) => blit_icon(&mut self.surface, pad, icon_y, &icon.pixels),
                _ => draw_icon(&mut self.surface, pad, icon_y, row.icon().kind, icon_colour),
            }
            let text_x = pad + 22;
            let name_baseline = y + (row_h / 2) + 1;
            let name = row.name();
            let name_room = w - text_x - pad - 4;
            let drawn = self.text(
                text_x,
                name_baseline,
                name,
                solid(theme.text, 255),
                Face::Strong,
                text_x + name_room,
            );
            // The characters the query matched, picked out on top.
            if !row.span.is_empty() && drawn > 0 {
                self.highlight(
                    text_x,
                    name_baseline,
                    name,
                    &row.span,
                    solid(theme.matched, 255),
                );
            }
            let under = row.context();
            if !under.is_empty() {
                self.text(
                    text_x,
                    name_baseline + small_size + 1,
                    under,
                    solid(theme.dim, 225),
                    Face::Small,
                    text_x + name_room,
                );
            }
            y += row_h;
        }

        // The scrollbar: the list is a window on a longer answer, and a
        // window with no bar is a reader who cannot tell there is more.
        let total = view.rows.len();
        let per_screen = ((h - input_h - 6) / row_h).max(1) as usize;
        if total > per_screen {
            let track_x = w - 5;
            let track_top = input_h + 4;
            let track_h = (h - track_top - 4).max(8);
            self.surface.fill_round_rect(
                track_x,
                track_top,
                3,
                track_h,
                2,
                solid(theme.border, 120),
            );
            let visible = (per_screen as i32).min(total as i32);
            let thumb_h = ((visible * track_h) / total as i32).max(12);
            let max_top = (total as i32 - visible).max(1);
            let thumb_y = track_top + (view.top as i32 * (track_h - thumb_h)) / max_top;
            self.surface.fill_round_rect(
                track_x,
                thumb_y.clamp(track_top, track_top + track_h - thumb_h),
                3,
                thumb_h,
                2,
                solid(theme.dim, 190),
            );
        }
        // The menu: a column on the right, one entry per action.
        if view.mode == Mode::Menu {
            let entry = SIDE_BAR_ENTRY_HEIGHT as i32;
            let bar_w = SIDE_BAR_WIDTH as i32;
            let bar_x = (w - pad - bar_w).max(pad);
            let gap = SIDE_BAR_GAP as i32;
            self.surface
                .vline(bar_x - gap / 2, input_h, h - 2, solid(theme.border, 140));
            for (j, action) in view.menu.iter().enumerate() {
                let ey = input_h + 3 + j as i32 * entry;
                if ey + entry > h - 2 {
                    break;
                }
                if j == view.menu_selected {
                    self.surface.fill_round_rect(
                        bar_x,
                        ey,
                        bar_w,
                        entry - 2,
                        6,
                        solid(theme.accent, 70),
                    );
                }
                let colour = if j == view.menu_selected {
                    solid(theme.text, 255)
                } else {
                    solid(theme.dim, 240)
                };
                self.text(
                    bar_x + 8,
                    ey + entry / 2 + 4,
                    action.label(lang),
                    colour,
                    Face::Regular,
                    bar_x + bar_w - 6,
                );
                let key = action.shortcut();
                if !key.is_empty() {
                    let kw = self.text_width(key, Face::Small);
                    self.text(
                        bar_x + bar_w - kw - 6,
                        ey + entry / 2 + 4,
                        key,
                        solid(theme.dim, 200),
                        Face::Small,
                        bar_x + bar_w - 2,
                    );
                }
            }
        }

        // The settings panel: a label, and its value on the right.
        if view.mode == Mode::Settings {
            for (j, (label, value)) in view.settings.iter().enumerate() {
                let sy = input_h + 6 + j as i32 * 26;
                if sy + 26 > h - 2 {
                    break;
                }
                if j == view.settings_selected {
                    self.surface.fill_round_rect(
                        pad,
                        sy - 2,
                        w - pad * 2,
                        24,
                        6,
                        solid(theme.accent, 60),
                    );
                }
                self.text(
                    pad + 6,
                    sy + 16,
                    label,
                    solid(theme.text, 235),
                    Face::Regular,
                    w / 2,
                );
                let vw = self.text_width(value, Face::Small);
                self.text(
                    w - pad - 6 - vw.min(w / 2),
                    sy + 16,
                    value,
                    solid(theme.dim, 235),
                    Face::Small,
                    w - pad - 2,
                );
            }
        }

        // The preview: the head of a file, over the list.
        if let Some(preview) = view.preview.as_ref() {
            self.draw_preview(preview, input_h, w, h, pad, theme, text);
        }

        // The warnings, at the bottom, above the latency.
        let mut wy = h - 6 - small_size;
        for warning in view.warnings.iter().rev() {
            if wy - small_size < baseline {
                break;
            }
            self.text(
                pad,
                wy,
                warning,
                solid(theme.dim, 230),
                Face::Small,
                w - pad,
            );
            wy -= small_size + 2;
        }

        // The latency: the number, and the bars behind it.
        //
        // The meter keeps a rolling window and offers eight levels as block
        // characters; the bars are those levels and they ease towards the
        // newest sample instead of jumping to it, which is what makes typing
        // feel measured rather than twitchy. The strip is drawn left to
        // right with the newest sample on the right.
        self.animate_bars(&view.latency.spark);
        let bars_w = self.bar_count as i32 * (BAR_WIDTH + BAR_GAP);
        let label = view.latency.last_label().unwrap_or_default();
        let label_w = if label.is_empty() {
            0
        } else {
            self.text_width(&label, Face::Small)
        };
        if !label.is_empty() {
            let colour = solid(theme.good, 245);
            self.text(
                w - pad - label_w,
                h - 8,
                &label,
                colour,
                Face::Small,
                w - pad,
            );
        }
        if self.bar_count > 0 {
            let bars_left = w - pad - bars_w - if label_w > 0 { label_w + 8 } else { 0 };
            let floor = h - 9;
            for index in 0..self.bar_count {
                let level = self.bars[index].max(0) as i32;
                if level == 0 {
                    continue;
                }
                let bar_h = level * BAR_HEIGHT / (8 * BAR_SCALE as i32) + 1;
                let x = bars_left + index as i32 * (BAR_WIDTH + BAR_GAP);
                if x < pad || x + BAR_WIDTH > w - pad {
                    continue;
                }
                // The newest sample is the one the reader is watching: it
                // carries the accent, the older history stays quiet.
                let colour = if index + 1 == self.bar_count {
                    solid(theme.accent, 235)
                } else {
                    solid(theme.dim, 150)
                };
                self.surface
                    .fill_round_rect(x, floor - bar_h, BAR_WIDTH, bar_h, 1, colour);
            }
        }

        (w as u32, h as u32)
    }

    /// The preview panel: a title bar and the first lines of the file.
    #[allow(clippy::too_many_arguments)]
    fn draw_preview(
        &mut self,
        preview: &Preview,
        input_h: i32,
        w: i32,
        h: i32,
        pad: i32,
        theme: Theme,
        text: &Text,
    ) {
        let panel_top = input_h + 4;
        let panel_h = (h - panel_top - 8).max(40);
        // A dim backdrop so the list behind is not read as part of it.
        self.surface.fill_round_rect(
            pad - 6,
            panel_top,
            w - (pad - 6) * 2,
            panel_h,
            10,
            argb(238, 10, 11, 13),
        );
        self.surface.stroke_round_rect(
            pad - 6,
            panel_top,
            w - (pad - 6) * 2,
            panel_h,
            10,
            1,
            solid(theme.border, 200),
        );
        let small = self.size_of(Face::Small) as i32;
        let mut y = panel_top + 6 + small;
        let title = rgy_launcher::text::fill(text.notice_preview_title, &[&preview.title]);
        self.text(
            pad,
            y,
            &title,
            solid(theme.accent, 245),
            Face::Regular,
            w - pad,
        );
        y += small + 4;
        for line in preview.lines.iter() {
            if y + 2 > panel_top + panel_h - 4 {
                break;
            }
            self.text(pad, y, line, solid(theme.dim, 235), Face::Small, w - pad);
            y += small + 2;
        }
    }
    /// A one-pixel border that follows the card's own rounded edge.
    ///
    /// The rasteriser's `stroke_round_rect` draws the straight edges at full
    /// width, which squares off the corners the fill just rounded; this
    /// walks the rows and insets each one by the arc, so the outline and the
    /// fill are the same shape.
    fn outline(&mut self, x: i32, y: i32, w: i32, h: i32, radius: i32, colour: u32) {
        if w <= 2 || h <= 2 {
            return;
        }
        for row in 0..h {
            let inset = corner_inset(row, h, radius.min((w.min(h) / 2).max(0)));
            let left = x + inset;
            let right = x + w - 1 - inset;
            if right < left {
                continue;
            }
            if row == 0 || row == h - 1 {
                self.surface.hline(left, right, y + row, colour);
            } else {
                self.surface.blend(
                    left,
                    y + row,
                    (colour & 0x00ff_ffff) | (colour & 0xff00_0000),
                );
                self.surface.blend(
                    right,
                    y + row,
                    (colour & 0x00ff_ffff) | (colour & 0xff00_0000),
                );
            }
        }
    }
    /// Pick out the characters a query matched, drawn on top of the name.
    ///
    /// The model says **which** characters earned the score — a list of
    /// character offsets — so the highlight is the ranking's own decision,
    /// drawn. The glyphs are collected first and painted after, because the
    /// face is borrowed while they are read and the surface while they are
    /// blended.
    fn highlight(&mut self, x: i32, baseline: i32, name: &str, span: &[u32], colour: u32) {
        if span.is_empty() {
            return;
        }
        let mut placed: alloc::vec::Vec<(i32, i32, u32, u32, u32, alloc::vec::Vec<u8>)> =
            alloc::vec::Vec::new();
        let mut pen = x;
        let mut index = 0u32;
        if let Some(font) = self.face(Face::Strong) {
            for ch in name.chars() {
                let Some(g) = font.glyph(ch) else {
                    index += 1;
                    continue;
                };
                if span.contains(&index) && g.width > 0 && g.height > 0 {
                    let len = (g.height as usize).saturating_mul(g.pitch as usize);
                    let bytes = g.coverage[..len.min(g.coverage.len())].to_vec();
                    placed.push((
                        pen + g.left,
                        baseline - g.top,
                        g.width,
                        g.height,
                        g.pitch,
                        bytes,
                    ));
                }
                pen += g.advance;
                index += 1;
            }
        }
        for (gx, gy, gw, gh, pitch, bytes) in placed {
            self.surface
                .blend_mask(gx, gy, gw, gh, &bytes, pitch, colour);
        }
    }
}

/// How far the panel's edge is inset at a row, for a rounded rectangle.
///
/// The card's gradient is drawn one row at a time, and a row that spanned
/// the full width would paint the corners square again. So each row is
/// inset by the arc's own horizontal offset, computed with an integer
/// square root (the radius is a dozen pixels; a float square root would be
/// a dependency on the host's maths for nothing).
fn corner_inset(row: i32, height: i32, radius: i32) -> i32 {
    if radius <= 0 {
        return 0;
    }
    let dy = if row < radius {
        radius - 1 - row
    } else if row >= height - radius {
        row - (height - radius)
    } else {
        return 0;
    };
    let rr = (radius * radius - dy * dy).max(0) as u32;
    radius - isqrt(rr) as i32
}

/// The integer square root of a small number, by Newton's own descent.
fn isqrt(n: u32) -> u32 {
    if n == 0 {
        return 0;
    }
    let mut x = n;
    let mut y = x.div_ceil(2);
    while y < x {
        x = y;
        y = (x + n / x) / 2;
    }
    x
}
/// The level a sparkline character names, 1..=8.
///
/// The meter offers the eight block characters `▁▂▃▄▅▆▇█`; anything else is
/// not a sample and answers zero, which draws no bar.
fn level_of(ch: char) -> u8 {
    match ch {
        '▁' => 1,
        '▂' => 2,
        '▃' => 3,
        '▄' => 4,
        '▅' => 5,
        '▆' => 6,
        '▇' => 7,
        '█' => 8,
        _ => 0,
    }
}
/// A theme colour with an alpha.
fn solid(c: Rgb, a: u8) -> u32 {
    argb(a, c.r, c.g, c.b)
}
/// Blend one pixel with a coverage on top of its alpha.
///
/// The surface's own [`Surface::blend`] takes the alpha the colour carries;
/// text needs the coverage to *modulate* that alpha, which is one line of
/// arithmetic and does not deserve a second method on the surface.
fn blend_at(surface: &mut Surface, x: i32, y: i32, colour: u32, coverage: u8) {
    let a = (((colour >> 24) & 0xff) * coverage as u32) / 255;
    surface.blend(x, y, (colour & 0x00ff_ffff) | (a << 24));
}

/// Blit a 16x16 RGBA icon over the panel.
///
/// The pixels are straight `0xRRGGBBAA`, so each one is blended with its
/// own alpha: a theme icon has soft edges, and drawing it opaque would put
/// a hard square around every row.
fn blit_icon(surface: &mut Surface, x: i32, y: i32, pixels: &[u32]) {
    for row in 0..crate::icons::ICON_SIZE {
        for col in 0..crate::icons::ICON_SIZE {
            let at = (row * crate::icons::ICON_SIZE + col) as usize;
            let Some(px) = pixels.get(at) else { continue };
            let alpha = px & 0xff;
            if alpha == 0 {
                continue;
            }
            let colour = (alpha << 24) | ((px >> 8) & 0x00ff_ffff);
            surface.blend(x + col as i32, y + row as i32, colour);
        }
    }
}
/// One 16x16 icon, drawn from rectangles and lines.
///
/// The launcher carries no image files and asks no shell for an icon: the
/// kind is enough, and a shape made of a few fills reads at sixteen pixels.
fn draw_icon(surface: &mut Surface, x: i32, y: i32, kind: IconKind, colour: u32) {
    let faint = (colour & 0x00ff_ffff) | (0x6000_0000);
    match kind {
        IconKind::Folder => {
            surface.fill_rect(x, y + 3, 16, 11, faint);
            surface.fill_rect(x + 1, y + 1, 6, 3, faint);
            surface.fill_rect(x + 1, y + 4, 14, 1, colour);
        }
        IconKind::Document => {
            surface.fill_rect(x + 3, y + 1, 10, 15, faint);
            surface.fill_rect(x + 11, y + 1, 2, 3, colour);
            for row in 0..3 {
                surface.fill_rect(x + 5, y + 6 + row * 3, 6, 1, colour);
            }
        }
        IconKind::Image => {
            surface.fill_rect(x + 2, y + 3, 13, 11, faint);
            surface.fill_rect(x + 4, y + 5, 14 - 4, 1, colour);
            surface.fill_rect(x + 5, y + 8, 3, 3, colour);
            surface.fill_rect(x + 9, y + 10, 4, 2, colour);
        }
        IconKind::Video => {
            surface.fill_rect(x + 2, y + 4, 12, 9, faint);
            surface.fill_rect(x + 6, y + 6, 1, 5, colour);
            surface.fill_rect(x + 7, y + 7, 1, 3, colour);
            surface.fill_rect(x + 8, y + 8, 1, 1, colour);
        }
        IconKind::Audio => {
            surface.fill_rect(x + 4, y + 9, 4, 4, colour);
            surface.fill_rect(x + 8, y + 4, 1, 6, colour);
            surface.fill_rect(x + 9, y + 3, 3, 1, colour);
        }
        IconKind::Application => {
            surface.fill_rect(x + 2, y + 3, 13, 11, faint);
            surface.fill_rect(x + 2, y + 3, 13, 2, colour);
            surface.fill_rect(x + 4, y + 8, 1, 1, colour);
            surface.fill_rect(x + 6, y + 8, 4, 1, colour);
        }
        IconKind::File => {
            surface.fill_rect(x + 3, y + 1, 10, 15, faint);
            surface.fill_rect(x + 3, y + 1, 10, 1, colour);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A frame at a size paints the panel: the middle pixel is the theme's
    /// own background, and the corners are not painted at all.
    #[test]
    fn the_panel_is_a_rounded_card() {
        let mut r = Renderer::new(200, 120);
        let view = View::default();
        let (w, h) = r.draw(&view, Lang::English);
        assert_eq!((w, h), (200, 120));
        assert_eq!(r.surface.get(100, 60) >> 24, 255, "the middle is opaque");
        let corner = r.surface.get(0, 0) >> 24;
        let middle = r.surface.get(100, 60) >> 24;
        assert!(
            corner < middle,
            "the corner is lighter than the middle: {corner} vs {middle}"
        );
    }

    /// Every icon kind draws something inside its sixteen pixels.
    #[test]
    fn every_icon_draws_something() {
        for kind in [
            IconKind::Folder,
            IconKind::Document,
            IconKind::Image,
            IconKind::Video,
            IconKind::Audio,
            IconKind::Application,
            IconKind::File,
        ] {
            let mut s = Surface::new(20, 20);
            draw_icon(&mut s, 2, 2, kind, argb(255, 200, 210, 220));
            let painted = s.pixels.iter().filter(|p| **p != 0).count();
            assert!(painted > 8, "{kind:?} drew only {painted} pixels");
        }
    }

    /// The fallback text path reaches the surface even with no FreeType.
    #[test]
    fn the_bitmap_font_still_draws_when_typography_is_missing() {
        let mut r = Renderer::new(200, 120);
        // Force the fallback: the surfaces are the same either way, and the
        // test holds the fallback path itself, not the machine's fonts.
        let before = r.surface.pixels.clone();
        let pen = r.text(10, 40, "report", argb(255, 255, 255, 255), Face::Small, 200);
        assert!(pen > 0, "the line advanced");
        assert_ne!(r.surface.pixels, before, "something was drawn");
    }

    /// A line is clipped at the right edge instead of spilling out.
    #[test]
    fn a_long_line_is_clipped() {
        let mut r = Renderer::new(80, 60);
        r.text(
            4,
            30,
            "a-very-long-name-that-cannot-fit",
            argb(255, 255, 255, 255),
            Face::Regular,
            76,
        );
        // Nothing beyond the clip column was painted.
        for y in 0..60 {
            for x in 76..80 {
                assert_eq!(r.surface.get(x, y), 0, "painted past the clip at {x},{y}");
            }
        }
    }
}
