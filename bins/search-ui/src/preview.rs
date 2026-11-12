// SPDX-License-Identifier: Apache-2.0

//! The preview: what is inside the file, in a terminal.
//!
//! The preview is the answer to "is this the one?", and it is answered by
//! reading the beginning of the file — a screenful of it, not the whole
//! thing. A launcher that read a four-gigabyte video to show a preview
//! would be a launcher nobody could use.
//!
//! The window has its own preview (`bins/rgxy/src/preview.rs`); this is the
//! terminal's, and it answers the same question with the same shape: the
//! name of the file and the lines that are in it.
//!
//! A file that is not text says so instead of showing noise: the bytes are
//! read, and a byte that is not text is what makes a file binary.

use std::io::Read;

use rgy_launcher::Preview;
use rgy_launcher::text::{Lang, Text};

/// How many bytes of the file the preview reads.
const MAX_BYTES: u64 = 64 * 1024;

/// How many lines the preview shows.
const MAX_LINES: usize = 12;

/// The beginning of a file.
///
/// A file that cannot be read has a preview that says so: an empty panel
/// would look like a bug, and a sentence does not.
///
/// The sentences are the vocabulary's, in the language the launcher read
/// once at start-up: the preview is part of a notice, and a notice is a
/// word of the product like the panel's title.
pub fn of(path: &str, lang: Lang) -> Preview {
    let text = Text::for_lang(lang);
    let title = name_of(path);
    let bytes = match read_head(path) {
        Ok(b) => b,
        Err(e) => {
            return Preview {
                title,
                lines: vec![text.file_not_read.replace("{e}", &e.to_string())],
            };
        }
    };

    if is_binary(&bytes) {
        return Preview {
            title,
            lines: vec![
                text.binary_file.replace(
                    "{}",
                    &std::fs::metadata(path)
                        .map(|m| m.len())
                        .unwrap_or(0)
                        .to_string(),
                ),
            ],
        };
    }

    let content = String::from_utf8_lossy(&bytes);
    let mut lines = Vec::new();
    for line in content.lines() {
        if lines.len() >= MAX_LINES {
            lines.push("…".to_string());
            break;
        }
        lines.push(line.to_string());
    }
    if lines.is_empty() {
        lines.push(text.empty_file.to_string());
    }
    Preview { title, lines }
}

/// The beginning of the file, and only the beginning.
///
/// The file is opened and the first [`MAX_BYTES`] are read: the preview
/// shows a screenful, and reading a whole video to cut it afterwards is
/// what froze the launcher and filled its memory.
fn read_head(path: &str) -> std::io::Result<Vec<u8>> {
    let mut file = std::fs::File::open(path)?;
    read_at_most(&mut file, MAX_BYTES)
}

/// At most `limit` bytes, and no more.
///
/// The cap is the whole point of this function: the reader is stopped at
/// the cap, and the bytes past it are never read — a file of two
/// gigabytes is read as the screenful the preview shows.
fn read_at_most<R: Read>(reader: &mut R, limit: u64) -> std::io::Result<Vec<u8>> {
    let mut bytes = Vec::new();
    reader.take(limit).read_to_end(&mut bytes)?;
    Ok(bytes)
}

/// The name of a path: the last component.
fn name_of(path: &str) -> String {
    let trimmed = path.trim_end_matches(['/', '\\']);
    match trimmed.rfind(['/', '\\']) {
        Some(i) => trimmed[i + 1..].to_string(),
        None => trimmed.to_string(),
    }
}

/// Whether the bytes are text.
///
/// A NUL byte is the sign: it cannot be in a text file, and it is in
/// every binary one that is worth telling apart from text.
fn is_binary(bytes: &[u8]) -> bool {
    bytes.contains(&0)
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A reader that never ends, and counts what it gave.
    ///
    /// The count is the point: a reader that gave more than the cap would
    /// be a preview that reads whole files.
    struct Endless {
        given: usize,
    }

    impl Read for Endless {
        fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
            for byte in buf.iter_mut() {
                *byte = b'a';
            }
            self.given += buf.len();
            Ok(buf.len())
        }
    }

    /// A path in the temp folder, for a test that needs a file.
    fn temp_path(tag: &str) -> std::path::PathBuf {
        std::env::temp_dir().join(format!(
            "rgy-search-ui-preview-{tag}-{}",
            std::process::id()
        ))
    }

    #[test]
    fn the_name_is_the_last_component() {
        assert_eq!(name_of("/home/dana/notas.txt"), "notas.txt");
        assert_eq!(name_of("C:\\Users\\dana\\notas.txt"), "notas.txt");
        assert_eq!(name_of("notas.txt"), "notas.txt");
    }

    #[test]
    fn a_nul_byte_makes_a_file_binary() {
        assert!(is_binary(b"a\0b"));
        assert!(!is_binary(b"a text file\n"));
    }

    #[test]
    fn a_file_that_is_not_there_says_so() {
        let preview = of("/tmp/rgy-search-ui-no-such-file-12345", Lang::English);
        assert!(!preview.lines.is_empty());
        assert!(
            preview.lines[0].contains("could not be read"),
            "{:?}",
            preview.lines
        );
    }

    #[test]
    fn the_preview_reads_at_most_the_cap() {
        // The read stops at the cap: the bytes past it are never read,
        // and a file of two gigabytes is read as the screenful the
        // preview shows.
        let mut reader = Endless { given: 0 };
        let bytes = read_at_most(&mut reader, MAX_BYTES).expect("the bytes are read");
        assert_eq!(bytes.len(), MAX_BYTES as usize);
        assert_eq!(
            reader.given, MAX_BYTES as usize,
            "nothing past the cap was read"
        );
    }

    #[test]
    fn the_preview_shows_the_first_lines_of_a_text_file() {
        let path = temp_path("notas.txt");
        std::fs::write(&path, "first line\nsegunda linha\n").expect("the file is written");
        let preview = of(path.to_str().expect("the path is text"), Lang::English);
        let _ = std::fs::remove_file(&path);
        assert!(preview.title.contains("notas.txt"), "{}", preview.title);
        assert_eq!(preview.lines, vec!["first line", "segunda linha"]);
    }

    #[test]
    fn a_binary_file_says_so_and_does_not_panic() {
        let path = temp_path("binario.bin");
        let mut data = vec![b'a'; 32];
        data.push(0);
        std::fs::write(&path, &data).expect("the file is written");
        let preview = of(path.to_str().expect("the path is text"), Lang::English);
        let _ = std::fs::remove_file(&path);
        assert!(preview.lines[0].contains("binary"), "{:?}", preview.lines);
    }

    #[test]
    fn an_empty_file_says_so() {
        let path = temp_path("vazio.txt");
        std::fs::write(&path, b"").expect("the file is written");
        let preview = of(path.to_str().expect("the path is text"), Lang::English);
        let _ = std::fs::remove_file(&path);
        assert!(preview.lines[0].contains("empty"), "{:?}", preview.lines);
    }
}
