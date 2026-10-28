// SPDX-License-Identifier: Apache-2.0

//! The preview: what is inside the file.
//!
//! The preview is the answer to "is this the one?", and it is answered by
//! reading the beginning of the file — a screenful of it, not the whole
//! thing. A launcher that read a four-gigabyte video to show a preview
//! would be a launcher nobody could use.
//!
//! A file that is not text says so instead of showing noise: the bytes
//! are read, and a byte that is not text is what makes a file binary.

use rgy_launcher::Preview;

/// How many bytes of the file the preview reads.
const MAX_BYTES: u64 = 64 * 1024;

/// How many lines the preview shows.
const MAX_LINES: usize = 12;

/// The preview of a file.
///
/// A file that cannot be read has a preview that says so: an empty panel
/// would look like a bug, and a sentence does not.
pub fn of(path: &str) -> Preview {
    let title = name_of(path);
    let bytes = match std::fs::read(path) {
        Ok(b) => b,
        Err(e) => {
            return Preview {
                title,
                lines: vec![format!("não foi possível ler o ficheiro: {e}")],
            };
        }
    };
    let bytes = &bytes[..bytes.len().min(MAX_BYTES as usize)];

    if is_binary(bytes) {
        return Preview {
            title,
            lines: vec![format!(
                "ficheiro binário · {} bytes",
                std::fs::metadata(path).map(|m| m.len()).unwrap_or(0)
            )],
        };
    }

    let text = String::from_utf8_lossy(bytes);
    let mut lines = Vec::new();
    for line in text.lines() {
        if lines.len() >= MAX_LINES {
            lines.push("…".to_string());
            break;
        }
        lines.push(line.to_string());
    }
    if lines.is_empty() {
        lines.push("(ficheiro vazio)".to_string());
    }
    Preview { title, lines }
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
        let preview = of("/tmp/rgy-rgxy-no-such-file-12345");
        assert!(!preview.lines.is_empty());
        assert!(
            preview.lines[0].contains("não foi possível"),
            "{:?}",
            preview.lines
        );
    }
}
