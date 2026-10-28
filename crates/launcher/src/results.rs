// SPDX-License-Identifier: Apache-2.0

//! The results, and the selection in them.
//!
//! The list is the engine's answer, in the engine's order — the ranking is
//! the product, and nothing here re-sorts it. What this owns is the
//! selection and the window onto the list: which row is highlighted, and
//! which rows the window shows when the list is longer than the window.

use alloc::string::String;
use alloc::vec::Vec;

/// One result row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Row {
    /// The whole path, as the platform spells it.
    pub path: String,
    /// The engine's score.
    pub score: i32,
    /// The byte offsets of the matched characters in the name.
    pub span: Vec<u32>,
    /// The line under the name, when the row is not a plain name.
    ///
    /// In the name layer this is nothing and the row shows its folder. In
    /// the content layer it is the line that matched — the thing the user
    /// is actually looking for — and the folder is what it replaces.
    pub context: Option<String>,
}

impl Row {
    /// A row with no span.
    pub fn new(path: String, score: i32) -> Row {
        Row {
            path,
            score,
            span: Vec::new(),
            context: None,
        }
    }

    /// The name: the last component of the path.
    ///
    /// Both separators are honoured, because the path may come from an index
    /// built on one platform and be shown on another.
    pub fn name(&self) -> &str {
        let trimmed = self.path.trim_end_matches(['/', '\\']);
        match trimmed.rfind(['/', '\\']) {
            Some(i) => &trimmed[i + 1..],
            None => trimmed,
        }
    }

    /// The folder that holds it: the path without the name.
    pub fn folder(&self) -> &str {
        let trimmed = self.path.trim_end_matches(['/', '\\']);
        match trimmed.rfind(['/', '\\']) {
            Some(i) => &trimmed[..i],
            None => "",
        }
    }

    /// The line under the name: the folder, or what the row says instead.
    ///
    /// The content layer puts the matched line there — the user is asking
    /// what is **inside** the file, and the folder does not answer that.
    pub fn context(&self) -> &str {
        match self.context.as_deref() {
            Some(text) => text,
            None => self.folder(),
        }
    }

    /// A row that shows `text` under the name.
    pub fn with_context(mut self, text: String) -> Row {
        self.context = Some(text);
        self
    }

    /// The name's characters that the query matched.
    ///
    /// The span is in the name's own bytes, so the caller has to fold the
    /// name before it can index with it — the same fold the engine used
    /// when it scored the match.
    pub fn matched(&self) -> &[u32] {
        &self.span
    }
}

/// The list of results and the selection in it.
#[derive(Debug, Clone, Default)]
pub struct Results {
    items: Vec<Row>,
    selected: usize,
    /// The first visible row.
    top: usize,
    /// How many rows the window shows.
    visible: usize,
}

impl Results {
    /// An empty list.
    ///
    /// The window shows everything: the list is only as tall as the results
    /// need, and the platform crate caps it when the screen says so.
    pub fn new() -> Results {
        Results {
            items: Vec::new(),
            selected: 0,
            top: 0,
            visible: usize::MAX,
        }
    }

    /// How many rows the window shows. Zero means one: a window that shows
    /// nothing cannot show the selection either.
    pub fn set_visible(&mut self, rows: usize) {
        self.visible = rows.max(1);
        self.clamp_window();
    }

    /// The rows.
    pub fn items(&self) -> &[Row] {
        &self.items
    }

    /// How many rows there are.
    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Whether there are no rows.
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Replace the list.
    ///
    /// The selection does not survive: the new list is a new answer, and a
    /// selection that pointed into the old one would point at a row that
    /// has nothing to do with what the user just asked.
    pub fn set(&mut self, items: Vec<Row>) {
        self.items = items;
        self.selected = 0;
        self.top = 0;
        self.clamp_window();
    }

    /// Empty the list.
    pub fn clear(&mut self) {
        self.set(Vec::new());
    }

    /// The selected row's index.
    pub fn selected(&self) -> Option<usize> {
        if self.items.is_empty() {
            None
        } else {
            Some(self.selected)
        }
    }

    /// The selected row.
    pub fn current(&self) -> Option<&Row> {
        self.items.get(self.selected)
    }

    /// Select the next row, and keep it in the window.
    pub fn select_next(&mut self) {
        if self.items.is_empty() {
            return;
        }
        if self.selected + 1 < self.items.len() {
            self.selected += 1;
        }
        self.clamp_window();
    }

    /// Select the previous row, and keep it in the window.
    pub fn select_previous(&mut self) {
        if self.items.is_empty() {
            return;
        }
        self.selected = self.selected.saturating_sub(1);
        self.clamp_window();
    }

    /// Select the first row.
    pub fn select_first(&mut self) {
        self.selected = 0;
        self.clamp_window();
    }

    /// Select the last row.
    pub fn select_last(&mut self) {
        self.selected = self.items.len().saturating_sub(1);
        self.clamp_window();
    }

    /// The first visible row, and how many rows the window shows.
    pub fn window(&self) -> (usize, usize) {
        if self.items.is_empty() {
            return (0, 0);
        }
        let count = self.visible.min(self.items.len());
        (self.top, count)
    }

    /// Keep the selection inside the window.
    fn clamp_window(&mut self) {
        if self.items.is_empty() {
            self.top = 0;
            self.selected = 0;
            return;
        }
        if self.selected >= self.items.len() {
            self.selected = self.items.len() - 1;
        }
        let count = self.visible.min(self.items.len());
        if self.selected < self.top {
            self.top = self.selected;
        } else if self.selected >= self.top + count {
            self.top = self.selected + 1 - count;
        }
        let max_top = self.items.len().saturating_sub(count);
        if self.top > max_top {
            self.top = max_top;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::string::ToString;

    fn rows(n: usize) -> Vec<Row> {
        (0..n).map(|i| Row::new(alloc::format!("/a/file{i}.txt"), i as i32)).collect()
    }

    #[test]
    fn an_empty_list_has_no_selection() {
        let r = Results::new();
        assert!(r.is_empty());
        assert_eq!(r.selected(), None);
        assert_eq!(r.current(), None);
        assert_eq!(r.window(), (0, 0));
    }

    #[test]
    fn the_selection_starts_on_the_first_row() {
        let mut r = Results::new();
        r.set(rows(5));
        assert_eq!(r.selected(), Some(0));
        assert_eq!(r.current().map(|x| x.score), Some(0));
    }

    #[test]
    fn the_selection_stops_at_the_ends() {
        let mut r = Results::new();
        r.set(rows(3));
        r.select_previous();
        assert_eq!(r.selected(), Some(0));
        r.select_next();
        r.select_next();
        r.select_next();
        assert_eq!(r.selected(), Some(2), "the last row stays selected");
    }

    #[test]
    fn the_window_scrolls_to_keep_the_selection_in_it() {
        let mut r = Results::new();
        r.set_visible(3);
        r.set(rows(10));
        assert_eq!(r.window(), (0, 3));
        r.select_next();
        r.select_next();
        r.select_next();
        assert_eq!(r.selected(), Some(3));
        assert_eq!(r.window(), (1, 3), "the window moved down by one");
        // And back up.
        r.select_previous();
        r.select_previous();
        r.select_previous();
        assert_eq!(r.selected(), Some(0));
        assert_eq!(r.window(), (0, 3), "the window went back to the top");
    }

    #[test]
    fn the_window_never_starts_past_the_end() {
        let mut r = Results::new();
        r.set_visible(5);
        r.set(rows(3));
        assert_eq!(r.window(), (0, 3), "a short list fills the window");
        r.select_last();
        assert_eq!(r.selected(), Some(2));
        assert_eq!(r.window(), (0, 3));
    }

    #[test]
    fn a_new_answer_drops_the_old_selection() {
        let mut r = Results::new();
        r.set(rows(5));
        r.select_last();
        assert_eq!(r.selected(), Some(4));
        r.set(rows(2));
        assert_eq!(r.selected(), Some(0), "the new list starts at the top");
    }

    #[test]
    fn the_name_is_the_last_component_and_the_folder_the_rest() {
        let r = Row::new("/home/dana/docs/relatorio.pdf".to_string(), 1);
        assert_eq!(r.name(), "relatorio.pdf");
        assert_eq!(r.folder(), "/home/dana/docs");
    }

    #[test]
    fn a_windows_path_has_a_name_too() {
        let r = Row::new("C:\\Users\\dana\\notas.txt".to_string(), 1);
        assert_eq!(r.name(), "notas.txt");
        assert_eq!(r.folder(), "C:\\Users\\dana");
    }

    #[test]
    fn the_line_under_the_name_is_the_folder_by_default() {
        let r = Row::new("/home/dana/docs/relatorio.pdf".to_string(), 1);
        assert_eq!(r.context(), "/home/dana/docs");
    }

    #[test]
    fn a_content_row_shows_the_line_that_matched() {
        // The content layer answers "what is inside the file", and the
        // folder does not answer that.
        let r = Row::new("/home/dana/docs/relatorio.pdf".to_string(), 1)
            .with_context("linha 42: o total do invoice".to_string());
        assert_eq!(r.context(), "linha 42: o total do invoice");
        assert_eq!(r.name(), "relatorio.pdf", "the name is still the name");
    }
}
