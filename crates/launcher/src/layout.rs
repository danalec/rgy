// SPDX-License-Identifier: Apache-2.0

//! The window's geometry.
//!
//! The numbers are the ones the launcher was drawn with: a compact window
//! a little over five hundred pixels wide, a single input field, and a
//! list that is only as tall as the results need.
//!
//! The window is **not** a fixed size. It grows with the number of
//! results, because a window with three results and a window with twenty
//! rows are different objects: the first is a search box, the second is a
//! browser. The height is therefore a function of the row count, and the
//! row count is the model's — the platform crate asks for the geometry it
//! is about to draw.

/// The launcher's geometry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Layout {
    /// The window's width, in pixels.
    pub width: u32,
    /// The height of the input field, in pixels.
    pub input_height: u32,
    /// The height of one result row, in pixels.
    pub row_height: u32,
    /// The padding around the content, in pixels.
    pub padding: u32,
    /// The corner radius, in pixels.
    pub radius: u32,
    /// The border's width, in pixels.
    pub border: u32,
    /// How many rows the list shows.
    pub rows: u32,
    /// How many lines the preview panel shows.
    ///
    /// Zero when there is no preview. The panel is a layer **over** the
    /// results, so it only makes the window taller when it is taller than
    /// the list it covers — a preview over twenty results is not a taller
    /// window.
    pub preview: u32,
}

impl Default for Layout {
    fn default() -> Layout {
        Layout {
            width: 600,
            input_height: 46,
            row_height: 34,
            padding: 14,
            radius: 12,
            border: 1,
            rows: 0,
            preview: 0,
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

    /// The window's height, in pixels.
    ///
    /// The padding counts once at the top and once at the bottom, and the
    /// border is inside them: a window's border is not extra space.
    ///
    /// The list is as tall as the taller of the two things that occupy it:
    /// the results and the preview panel. A preview over a short list
    /// makes the window taller; a preview over a long one does not.
    pub fn height(&self) -> u32 {
        let list = if self.preview > self.rows {
            self.preview
        } else {
            self.rows
        };
        self.padding * 2 + self.input_height + list * self.row_height + self.border * 2
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
        assert_eq!(l.height() - before, 4 * l.row_height);
    }
}
