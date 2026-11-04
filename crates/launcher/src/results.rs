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
    /// The character offsets of the matched characters in the name.
    pub span: Vec<u32>,
    /// The line under the name, when the row is not a plain name.
    ///
    /// In the name layer this is nothing and the row shows its folder. In
    /// the content layer it is the line that matched — the thing the user
    /// is actually looking for — and the folder is what it replaces.
    pub context: Option<String>,
    /// Whether the result is a folder.
    ///
    /// The source knows and the path does not say: a folder's path is
    /// spelled like a file's. The row carries the answer so that the icon
    /// beside the name is the icon of the thing.
    pub is_folder: bool,
}

impl Row {
    /// A row with no span.
    pub fn new(path: String, score: i32) -> Row {
        Row {
            path,
            score,
            span: Vec::new(),
            context: None,
            is_folder: false,
        }
    }

    /// A row that knows it is a folder.
    pub fn with_folder(mut self, is_folder: bool) -> Row {
        self.is_folder = is_folder;
        self
    }

    /// The kind of thing the row is, for the icon beside it.
    ///
    /// The flag comes first: the source said, and the source looked at
    /// the index. The spelling is the fallback for a row whose source did
    /// not say — a path that ends in a separator is a folder the way the
    /// platform spells one.
    pub fn icon(&self) -> IconKind {
        if self.is_folder {
            return IconKind::Folder;
        }
        IconKind::of(&self.path)
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
    /// The span counts the characters of the name from the first one —
    /// the first character is 0 — and it counts the name as it is
    /// written: the engine folded the name to score it, and the
    /// positions it sends are the positions in the name the user sees.
    /// A caller that folded the name again would be looking at a
    /// different text.
    pub fn matched(&self) -> &[u32] {
        &self.span
    }
}

/// The kind of thing a result is, for the icon beside it.
///
/// The launcher draws one icon per result, and the icon is the system's
/// own — so the model only has to say *what kind of thing* the result
/// is, and the platform crate asks the shell for the icon that goes
/// with it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IconKind {
    /// A folder.
    Folder,
    /// A document: a text, a PDF, an office file.
    Document,
    /// An image.
    Image,
    /// A video.
    Video,
    /// A sound file.
    Audio,
    /// Something the machine runs.
    Application,
    /// A file of no kind of its own.
    File,
}

impl IconKind {
    /// The kind of a path, from the way it is spelled.
    ///
    /// This is the fallback for a row whose source did not say: the row
    /// knows whether the result is a folder (see [`Row::icon`]), and a
    /// path that ends in a separator is a directory the way the platform
    /// spells one. Everything else is told apart by its extension.
    ///
    /// The extension is compared without regard to case, because
    /// `README.MD` and `readme.md` are the same file to a person, and
    /// the shell would answer with the same icon for both.
    pub fn of(path: &str) -> IconKind {
        let trimmed = path.trim_end_matches(['/', '\\']);
        if trimmed.len() != path.len() {
            return IconKind::Folder;
        }
        let name = match trimmed.rfind(['/', '\\']) {
            Some(i) => &trimmed[i + 1..],
            None => trimmed,
        };
        // A dot at the start of the name is part of the name
        // (`.gitignore`), and a dot at the end is nothing at all.
        match name.rfind('.') {
            Some(i) if i > 0 && i + 1 < name.len() => IconKind::of_extension(&name[i + 1..]),
            _ => IconKind::File,
        }
    }

    /// The kind an extension names.
    fn of_extension(ext: &str) -> IconKind {
        if DOCUMENT_EXTENSIONS
            .iter()
            .any(|e| ext.eq_ignore_ascii_case(e))
        {
            IconKind::Document
        } else if IMAGE_EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e)) {
            IconKind::Image
        } else if VIDEO_EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e)) {
            IconKind::Video
        } else if AUDIO_EXTENSIONS.iter().any(|e| ext.eq_ignore_ascii_case(e)) {
            IconKind::Audio
        } else if APPLICATION_EXTENSIONS
            .iter()
            .any(|e| ext.eq_ignore_ascii_case(e))
        {
            IconKind::Application
        } else {
            IconKind::File
        }
    }
}

/// The extensions that make a document.
const DOCUMENT_EXTENSIONS: &[&str] = &[
    "csv", "doc", "docx", "epub", "md", "markdown", "odt", "pdf", "ppt", "pptx", "rtf", "tex",
    "txt", "xls", "xlsx",
];

/// The extensions that make an image.
const IMAGE_EXTENSIONS: &[&str] = &[
    "bmp", "gif", "ico", "jpeg", "jpg", "png", "svg", "tif", "tiff", "webp",
];

/// The extensions that make a video.
const VIDEO_EXTENSIONS: &[&str] = &["avi", "flv", "m4v", "mkv", "mov", "mp4", "webm", "wmv"];

/// The extensions that make a sound file.
const AUDIO_EXTENSIONS: &[&str] = &["aac", "flac", "m4a", "mp3", "ogg", "opus", "wav", "wma"];

/// The extensions that make something the machine runs.
const APPLICATION_EXTENSIONS: &[&str] = &["bat", "cmd", "com", "exe", "lnk", "ps1", "scr"];

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
        (0..n)
            .map(|i| Row::new(alloc::format!("/a/file{i}.txt"), i as i32))
            .collect()
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
        let r = Row::new("/home/dana/docs/report.pdf".to_string(), 1);
        assert_eq!(r.name(), "report.pdf");
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
        let r = Row::new("/home/dana/docs/report.pdf".to_string(), 1);
        assert_eq!(r.context(), "/home/dana/docs");
    }

    #[test]
    fn a_content_row_shows_the_line_that_matched() {
        // The content layer answers "what is inside the file", and the
        // folder does not answer that.
        let r = Row::new("/home/dana/docs/report.pdf".to_string(), 1)
            .with_context("line 42: the invoice total".to_string());
        assert_eq!(r.context(), "line 42: the invoice total");
        assert_eq!(r.name(), "report.pdf", "the name is still the name");
    }

    #[test]
    fn a_path_that_ends_in_a_separator_is_a_folder() {
        assert_eq!(IconKind::of("C:\\Users\\dana\\Docs\\"), IconKind::Folder);
        assert_eq!(IconKind::of("/home/dana/docs/"), IconKind::Folder);
    }

    /// A folder with a file's spelling is still a folder.
    ///
    /// The path of a folder is spelled like the path of a file, and the
    /// index is what says which is which: a row that was told is a folder
    /// shows the folder's icon, and a row that was not told falls back to
    /// the spelling.
    #[test]
    fn the_flag_says_what_the_spelling_cannot() {
        let folder = Row::new("C:\\Users\\dana\\Docs".to_string(), 1).with_folder(true);
        assert_eq!(folder.icon(), IconKind::Folder);
        // Without the flag the same spelling is a file with no extension.
        let file = Row::new("C:\\Users\\dana\\Docs".to_string(), 1);
        assert_eq!(file.icon(), IconKind::File);
        // And the extension still says what the file is.
        let doc = Row::new("C:\\a\\relatorio.docx".to_string(), 1);
        assert_eq!(doc.icon(), IconKind::Document);
    }

    #[test]
    fn the_extension_says_what_the_file_is() {
        assert_eq!(IconKind::of("/a/relatorio.docx"), IconKind::Document);
        assert_eq!(IconKind::of("/a/foto.png"), IconKind::Image);
        assert_eq!(IconKind::of("/a/filme.mp4"), IconKind::Video);
        assert_eq!(IconKind::of("/a/musica.mp3"), IconKind::Audio);
        assert_eq!(IconKind::of("/a/programa.exe"), IconKind::Application);
    }

    #[test]
    fn the_extension_is_not_case_sensitive() {
        assert_eq!(IconKind::of("/a/FOTO.PNG"), IconKind::Image);
        assert_eq!(IconKind::of("C:\\a\\Relatorio.PDF"), IconKind::Document);
    }

    #[test]
    fn a_name_without_an_extension_is_a_plain_file() {
        assert_eq!(IconKind::of("/a/README"), IconKind::File);
        assert_eq!(IconKind::of("/a/notas."), IconKind::File);
    }

    #[test]
    fn a_dot_at_the_start_of_the_name_is_not_an_extension() {
        assert_eq!(IconKind::of("/a/.gitignore"), IconKind::File);
    }

    #[test]
    fn an_extension_nobody_knows_is_a_plain_file() {
        assert_eq!(IconKind::of("/a/coisa.qqq"), IconKind::File);
    }
}
