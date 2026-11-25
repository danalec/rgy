// SPDX-License-Identifier: Apache-2.0

//! The window's geometry.
//!
//! The numbers are the ones the launcher was drawn with: a compact window
//! a little over five hundred units wide, a single input field, and a
//! list that is only as tall as the results need.
//!
//! The window is **not** a fixed size. It grows with the number of
//! results, because a window with three results and a window with twenty
//! rows are different objects: the first is a search box, the second is a
//! browser. The height is therefore a function of the row count, and the
//! row count is the model's — the platform crate asks for the geometry it
//! is about to draw.

/// The DPI of a screen that is not scaled.
///
/// The geometry of the launcher is in the units of a 96-DPI screen, and
/// this is that screen's own DPI: the scale of another screen is its
/// DPI over this one.
pub const BASE_DPI: u32 = 96;

/// The scale of a screen, in per cent.
///
/// The geometry is in the model's units and the window is in device
/// pixels, and the two are the same number only on a 96-DPI screen. The
/// scale is the ratio between the two, and it is kept in per cent
/// because the ratio is not always a whole number — a 144-DPI screen is
/// one and a half, and a scale that dropped the half would make the
/// window two thirds of the size it was drawn for.
pub const SCALE_PERCENT: u32 = 100;

/// The width of the side bar, in the model's units.
///
/// The actions of the selected result are a column on the **right** of the
/// window, and the column is this wide: enough for the longest label and
/// its key, and not more. The window is six hundred units wide and the
/// results need the rest — the side bar is inside the window, not beside
/// it, because a window that grows when the actions open is a window that
/// moves under the user's hands.
pub const SIDE_BAR_WIDTH: u32 = 190;

/// The gap between the results and the side bar, in the model's units.
///
/// The two are separate things and the gap says so: a column that touches
/// the names reads as part of them.
pub const SIDE_BAR_GAP: u32 = 12;

/// The height of one entry of the side bar, in the model's units.
pub const SIDE_BAR_ENTRY_HEIGHT: u32 = 30;

/// The shortest an entry of the side bar may be, in the model's units.
///
/// An entry holds a line of text and the margin under it, and an entry
/// shorter than that would cut the label in half.
pub const SIDE_BAR_MINIMUM_ENTRY_HEIGHT: u32 = 24;

/// The inset above and below the settings panel's rows, in the model's units.
///
/// The picture draws the first row this far under the panel's top and
/// leaves the same room under the last one. It is the geometry's number as
/// well as the picture's: a window measured without it is a window whose
/// last row the picture has no room to draw, and the row the arrows can
/// still reach is a row the user cannot see.
pub const PANEL_INSET: u32 = 6;

/// One model unit, in the device pixels of a screen at this scale.
///
/// The scale is in per cent (see [`SCALE_PERCENT`]). The answer is a
/// whole number of device pixels — a pixel is the smallest thing the
/// platform draws — and it is the nearest one, so that a scale which is
/// not a whole number does not shrink the window a little every time.
pub fn device_pixels(units: u32, scale: u32) -> u32 {
    (units.saturating_mul(scale) + SCALE_PERCENT / 2) / SCALE_PERCENT
}

/// A number of device pixels, in the model's units.
///
/// The model's units are the units of a 96-DPI screen, and the screen
/// is not always that one: the pixels of a 192-DPI screen are two of
/// the model's for each one of theirs.
pub fn model_pixels(pixels: u32, scale: u32) -> u32 {
    pixels.saturating_mul(SCALE_PERCENT) / scale.max(1)
}

/// The launcher's geometry.
///
/// The numbers are in the **model's units** — the units of a 96-DPI
/// screen — and not in the device pixels of the screen the window is
/// drawn on: the window multiplies them by the screen's own scale (see
/// [`Layout::scaled`]), and the model never learns the scale.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The window's width, in the model's units.
    pub width: u32,
    /// The height of the input field, in the model's units.
    ///
    /// The field holds the query **and the line under it**: the query is
    /// drawn fourteen units down and the line under it ends six units
    /// above the field's bottom. A field that holds only the query hides
    /// the line under the first row's band, and the line is what says
    /// which question the rows answer.
    pub input_height: u32,
    /// The height of one result row, in the model's units.
    pub row_height: u32,
    /// The padding around the content, in the model's units.
    pub padding: u32,
    /// The corner radius, in the model's units.
    pub radius: u32,
    /// The border's width, in the model's units.
    pub border: u32,
    /// How many rows the list shows.
    pub rows: u32,
    /// How many lines the preview panel shows.
    ///
    /// Zero when there is no preview. The panel is a layer **over** the
    /// results, so it only makes the window taller when it is taller than
    /// the list it covers — a preview over twenty results is not a taller
    /// window.
    ///
    /// The count is of **lines**: the panel's title is one of them, and
    /// the lines of the file under it are the rest.
    pub preview: u32,
    /// Where the preview panel's title starts, in the model's units.
    pub preview_title_top: u32,
    /// The height of the preview panel's header — its title and the gap
    /// under it — in the model's units.
    pub preview_header_height: u32,
    /// The height of one line of the preview, in the model's units.
    ///
    /// The panel's lines are eighteen units apart and not a row tall: a
    /// row is a result with an icon and a folder under it, and a panel
    /// measured in rows is a hundred units taller than its own text.
    pub preview_line_height: u32,
    /// The width of the side bar, in the model's units.
    ///
    /// Zero when the actions are not shown. The side bar is the column of
    /// actions on the right of the window, and the results are what is
    /// left of the window's width once it is there.
    pub side_bar: u32,
    /// The gap between the results and the side bar, in the model's units.
    ///
    /// A field and not a constant: the gap is part of the picture, and a
    /// picture is drawn at the screen's scale.
    pub side_bar_gap: u32,
    /// How many rows the settings panel draws, in the model's units.
    ///
    /// Zero when no panel is open. The panel is a layer **over** the
    /// results, like the preview, so it only makes the window taller
    /// when it is taller than the list it covers — and a command line
    /// has no results at all, which is why the panel has to bring its
    /// own room.
    pub panel: u32,
}

impl Default for Layout {
    fn default() -> Layout {
        Layout {
            width: 600,
            input_height: 59,
            row_height: 34,
            padding: 14,
            radius: 12,
            border: 1,
            rows: 0,
            preview: 0,
            preview_title_top: 5,
            preview_header_height: 34,
            preview_line_height: 18,
            side_bar: 0,
            side_bar_gap: SIDE_BAR_GAP,
            panel: 0,
        }
    }
}

impl Layout {
    /// The geometry with a given number of rows.
    pub fn with_rows(rows: u32) -> Layout {
        Layout {
            rows,
            ..Layout::default()
        }
    }

    /// The geometry with a width of the user's choosing.
    ///
    /// The width is the one dimension the user has an opinion about: the
    /// height is the rows', and the rows are the screen's. A wider window
    /// shows a longer name, and that is the whole of what widening does.
    pub fn with_width(mut self, width: u32) -> Layout {
        self.width = width;
        self
    }

    /// The geometry at a screen's scale.
    ///
    /// The scale is in per cent (see [`SCALE_PERCENT`]). The pixel
    /// fields are multiplied by it and the counts are not: a row is a
    /// row, and half a row is not a thing.
    pub fn scaled(self, scale: u32) -> Layout {
        Layout {
            width: device_pixels(self.width, scale),
            input_height: device_pixels(self.input_height, scale),
            row_height: device_pixels(self.row_height, scale),
            padding: device_pixels(self.padding, scale),
            radius: device_pixels(self.radius, scale),
            border: device_pixels(self.border, scale),
            rows: self.rows,
            preview: self.preview,
            preview_title_top: device_pixels(self.preview_title_top, scale),
            preview_header_height: device_pixels(self.preview_header_height, scale),
            preview_line_height: device_pixels(self.preview_line_height, scale),
            side_bar: device_pixels(self.side_bar, scale),
            side_bar_gap: device_pixels(self.side_bar_gap, scale),
            panel: self.panel,
        }
    }

    /// The window's height, in the model's units.
    ///
    /// The padding counts once at the top and once at the bottom, and the
    /// border is inside them: a window's border is not extra space.
    ///
    /// The list is as tall as the taller of the two things that occupy it:
    /// the results and the preview panel — and the two are measured in
    /// their own units, because a row of results is a row tall and a
    /// line of the preview is eighteen units tall. A preview over a
    /// short list makes the window taller; a preview over a long one
    /// does not.
    pub fn height(&self) -> u32 {
        self.padding * 2 + self.input_height + self.list_height() + self.border * 2
    }

    /// The height the list has, in the model's units.
    ///
    /// The list is as tall as the taller of the results, the preview
    /// panel and the settings panel, and the column of actions lives in
    /// it: the column takes the room the results give up and the window
    /// does not grow.
    pub fn list_height(&self) -> u32 {
        (self.rows * self.row_height)
            .max(self.preview_height())
            .max(self.panel_height())
    }

    /// The height the settings panel needs, in the model's units.
    ///
    /// The panel is a list of rows with an inset above and below them,
    /// and it is measured the way the picture draws it: one row per
    /// setting, the first one an inset under the panel's top, and the
    /// last one an inset above the panel's bottom. Zero when no panel is
    /// open, so a window with no panel is the results' size.
    pub fn panel_height(&self) -> u32 {
        if self.panel == 0 {
            return 0;
        }
        self.panel * self.row_height + PANEL_INSET * 2
    }

    /// Whether the column of actions has room in this window.
    ///
    /// The column lives in the room the results give up, and its entries
    /// are as tall as they can be and no taller. A window with one result
    /// is shorter than a column of four entries, and a column that does
    /// not fit is not drawn half of itself.
    ///
    /// The rule is here and not in the two places that need it: the model
    /// and the picture have to agree about the column, and two answers to
    /// the same question are two answers.
    pub fn column_fits(&self, entries: u32) -> bool {
        if entries == 0 {
            return false;
        }
        self.list_height() >= entries * SIDE_BAR_MINIMUM_ENTRY_HEIGHT
    }

    /// The height the preview panel needs, in the model's units.
    ///
    /// The panel is a header and a stack of lines: the count of lines is
    /// a count of lines and not a count of rows, and a panel measured in
    /// rows is a hundred units taller than the text it shows.
    pub fn preview_height(&self) -> u32 {
        if self.preview == 0 {
            return 0;
        }
        // The title is one of the lines the count names.
        self.preview_header_height + (self.preview - 1) * self.preview_line_height
    }

    /// The top of the input field.
    pub fn input_top(&self) -> u32 {
        self.border + self.padding
    }

    /// The top of the result list.
    pub fn list_top(&self) -> u32 {
        self.input_top() + self.input_height
    }

    /// The top of one row, counted from the window's top.
    pub fn row_top(&self, index: u32) -> u32 {
        self.list_top() + index * self.row_height
    }

    /// The left edge of the content.
    pub fn content_left(&self) -> u32 {
        self.border + self.padding
    }

    /// The width the content has.
    pub fn content_width(&self) -> u32 {
        self.width.saturating_sub((self.border + self.padding) * 2)
    }

    /// The geometry with a side bar of this width.
    ///
    /// The width is the caller's: the model says whether the actions are
    /// shown, and the geometry says how much room they take.
    pub fn with_side_bar(mut self, width: u32) -> Layout {
        self.side_bar = width;
        self
    }

    /// The width of the side bar, in the model's units.
    ///
    /// Zero when the actions are not shown, and then the results have the
    /// whole of the content's width.
    pub fn side_bar_width(&self) -> u32 {
        self.side_bar
    }

    /// The width the results have, in the model's units.
    ///
    /// The list is the content's width less the side bar and the gap
    /// between them: the side bar is inside the window, so the results
    /// give up the room and the window does not grow.
    pub fn list_width(&self) -> u32 {
        if self.side_bar == 0 {
            // No column, no gap: the gap is between the names and the
            // column, and there is no column.
            return self.content_width();
        }
        self.content_width()
            .saturating_sub(self.side_bar)
            .saturating_sub(self.side_bar_gap)
    }

    /// The left edge of the side bar, in the model's units.
    ///
    /// The side bar ends at the window's own margin, so the column is
    /// flush with the right of the content and the gap is on its left.
    pub fn side_bar_left(&self) -> u32 {
        self.width
            .saturating_sub(self.border + self.padding + self.side_bar)
    }

    /// The right edge of the side bar, in the model's units.
    pub fn side_bar_right(&self) -> u32 {
        self.width.saturating_sub(self.border + self.padding)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_window_with_no_results_is_the_input_field_alone() {
        let l = Layout::with_rows(0);
        assert_eq!(l.height(), l.input_height + l.padding * 2 + l.border * 2);
    }

    #[test]
    fn the_window_grows_with_the_results() {
        let empty = Layout::with_rows(0);
        let ten = Layout::with_rows(10);
        assert_eq!(
            ten.height() - empty.height(),
            10 * empty.row_height,
            "ten rows are ten rows tall"
        );
    }

    #[test]
    fn the_list_starts_under_the_field() {
        let l = Layout::with_rows(3);
        assert_eq!(l.list_top(), l.input_top() + l.input_height);
        assert_eq!(l.row_top(0), l.list_top());
        assert_eq!(l.row_top(2), l.list_top() + 2 * l.row_height);
    }

    #[test]
    fn the_content_fits_inside_the_window() {
        let l = Layout::with_rows(5);
        assert!(l.content_width() > 0);
        assert!(l.content_width() < l.width);
    }

    #[test]
    fn a_preview_over_a_long_list_does_not_make_the_window_taller() {
        let mut l = Layout::with_rows(10);
        let before = l.height();
        l.preview = 4;
        assert_eq!(l.height(), before, "the panel is over the results");
    }

    #[test]
    fn a_preview_over_a_short_list_makes_the_window_taller() {
        let mut l = Layout::with_rows(1);
        let before = l.height();
        l.preview = 5;
        assert_eq!(l.height() - before, l.preview_height() - l.row_height);
    }

    /// The preview is measured in lines and not in rows.
    ///
    /// A panel of a title and thirteen lines is a header and thirteen
    /// lines of eighteen units each, and a window that asked for
    /// thirteen rows of thirty-four units was two hundred units taller
    /// than the text it was showing.
    #[test]
    fn a_preview_of_lines_is_not_a_preview_of_rows() {
        let mut l = Layout::with_rows(1);
        // A title and thirteen lines under it.
        l.preview = 14;
        let panel = 34 + 13 * 18;
        assert_eq!(
            l.height(),
            l.padding * 2 + l.input_height + panel + l.border * 2,
            "the panel is as tall as the text it shows"
        );
    }

    /// The panel's own height: a header and the lines under it.
    #[test]
    fn the_panel_is_a_header_and_its_lines() {
        let mut l = Layout::with_rows(3);
        assert_eq!(l.preview_height(), 0, "no panel, no height");
        l.preview = 1;
        assert_eq!(l.preview_height(), 34, "a panel with only a title");
        l.preview = 3;
        assert_eq!(l.preview_height(), 34 + 2 * 18);
    }

    /// The geometry at the scale of the screen it is drawn on.
    ///
    /// The window is in device pixels and the model is in the units of a
    /// 96-DPI screen, so the geometry is multiplied by the screen's own
    /// scale: a screen twice as big gets a window twice as big, and the
    /// rows are still rows.
    #[test]
    fn the_geometry_is_the_model_s_geometry_at_the_screen_s_scale() {
        let model = Layout::with_rows(4);
        let twice = model.scaled(SCALE_PERCENT * 2);
        assert_eq!(twice.width, model.width * 2);
        assert_eq!(twice.input_height, model.input_height * 2);
        assert_eq!(twice.row_height, model.row_height * 2);
        assert_eq!(twice.padding, model.padding * 2);
        assert_eq!(twice.radius, model.radius * 2);
        assert_eq!(twice.border, model.border * 2);
        assert_eq!(twice.rows, model.rows, "a row is a row");
        assert_eq!(twice.preview, model.preview, "and so is a count of lines");
        assert_eq!(twice.preview_title_top, model.preview_title_top * 2);
        assert_eq!(twice.preview_header_height, model.preview_header_height * 2);
        assert_eq!(twice.preview_line_height, model.preview_line_height * 2);
    }

    /// A screen that is not scaled is the screen the geometry was drawn
    /// for, and nothing changes there.
    #[test]
    fn an_unscaled_screen_changes_nothing() {
        let model = Layout::with_rows(4);
        assert_eq!(model.scaled(SCALE_PERCENT), model);
    }

    /// The scale of a screen is not always a whole number.
    ///
    /// A 144-DPI screen is one and a half, and a scale that dropped the
    /// half would make the window two thirds of the size it was drawn
    /// for — and the text one and a half times the size, which does not
    /// fit in it.
    #[test]
    fn a_screen_that_is_not_whole_is_scaled_all_the_same() {
        let model = Layout::with_rows(4);
        let one_and_a_half = model.scaled(SCALE_PERCENT + SCALE_PERCENT / 2);
        assert_eq!(one_and_a_half.width, model.width * 3 / 2);
        assert_eq!(one_and_a_half.row_height, model.row_height * 3 / 2);
        assert!(one_and_a_half.row_height > model.row_height);
    }

    /// The pixels of a screen are the model's units at its own scale.
    #[test]
    fn the_pixels_of_a_screen_are_the_model_s_units_at_its_scale() {
        assert_eq!(device_pixels(600, SCALE_PERCENT), 600);
        assert_eq!(device_pixels(600, SCALE_PERCENT * 2), 1200);
        assert_eq!(model_pixels(1200, SCALE_PERCENT * 2), 600);
        assert_eq!(model_pixels(600, SCALE_PERCENT * 2), 300);
    }

    /// Without the actions there is no side bar, and the results have the
    /// whole of the content.
    #[test]
    fn a_window_without_the_actions_has_no_side_bar() {
        let l = Layout::with_rows(4);
        assert_eq!(l.side_bar_width(), 0);
        assert_eq!(l.list_width(), l.content_width());
    }

    /// The side bar is inside the window: the results give up the room and
    /// the window does not grow.
    #[test]
    fn the_side_bar_is_inside_the_window() {
        let l = Layout::with_rows(4).with_side_bar(SIDE_BAR_WIDTH);
        assert_eq!(l.width, 600, "the window did not grow");
        assert_eq!(l.side_bar_width(), SIDE_BAR_WIDTH);
        assert_eq!(
            l.list_width(),
            l.content_width() - SIDE_BAR_WIDTH - SIDE_BAR_GAP,
            "the results gave up the room of the column"
        );
    }

    /// The column ends at the window's own margin, and the gap is on its
    /// left: a column that touches the names reads as part of them.
    #[test]
    fn the_column_is_flush_with_the_right_margin() {
        let l = Layout::with_rows(4).with_side_bar(SIDE_BAR_WIDTH);
        assert_eq!(l.side_bar_right(), l.width - l.border - l.padding);
        assert_eq!(
            l.side_bar_left() + SIDE_BAR_WIDTH,
            l.side_bar_right(),
            "the column is as wide as it says"
        );
        assert_eq!(
            l.side_bar_left(),
            l.content_left() + l.list_width() + l.side_bar_gap,
            "the gap is between the names and the column"
        );
    }

    /// A side bar wider than the window leaves the results nothing, and
    /// nothing is not a negative width.
    #[test]
    fn a_side_bar_wider_than_the_window_leaves_nothing() {
        let l = Layout::with_rows(4).with_side_bar(10_000);
        assert_eq!(l.list_width(), 0, "the names have nowhere to be");
        assert!(
            l.side_bar_left() <= l.side_bar_right(),
            "the column does not start after it ends"
        );
    }

    /// The column needs room for its shortest entries, and it takes it
    /// from the list.
    #[test]
    fn the_column_needs_room_for_its_shortest_entries() {
        let one = Layout::with_rows(1);
        assert!(
            !one.column_fits(4),
            "one result is not room for four entries"
        );
        let three = Layout::with_rows(3);
        assert!(three.column_fits(4), "three results are");
        assert!(three.column_fits(3), "and three entries are easier");
    }

    /// A column with no entries is not drawn at all.
    #[test]
    fn a_column_with_no_entries_is_not_drawn() {
        assert!(!Layout::with_rows(10).column_fits(0));
    }

    /// The column is scaled like the window.
    #[test]
    fn the_column_is_scaled_like_the_window() {
        let model = Layout::with_rows(4).with_side_bar(SIDE_BAR_WIDTH);
        let twice = model.scaled(SCALE_PERCENT * 2);
        assert_eq!(twice.side_bar, SIDE_BAR_WIDTH * 2);
        assert_eq!(twice.list_width(), model.list_width() * 2);
    }
}
