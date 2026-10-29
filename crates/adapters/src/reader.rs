// SPDX-License-Identifier: Apache-2.0

//! The format adapters, as a reader for the content path.
//!
//! **Why a reader and not a function.** The content path owns the reading,
//! the accounting and the merge — the limit, the budget, the ranking, the
//! files that were skipped — and it owns them because it owns them for the
//! plain path too. What it cannot do is read a file an adapter claims: the
//! adapters are `std`, the content engine is `no_std`, and the two cannot
//! be linked the other way round. So the engine asks a question and this
//! crate answers it: **what does this file render to?** A file no adapter
//! claims renders to nothing, and the engine reads its own bytes then, as
//! it always did.

use rgx_core::{Ctx, Registry};
use rgy_name_core::content::{self, AdapterReader, Rendered};
use std::sync::OnceLock;

use crate::registry;

/// The format adapters, as a reader for the content path.
///
/// The content path asks per file and this is where the registry answers.
pub struct Adapters;

impl AdapterReader for Adapters {
    /// The documents the adapters rendered for `path`.
    ///
    /// An empty vector means no adapter claims the file: the content path
    /// reads the file's own bytes then, exactly as it always did.
    fn rendered(&self, path: &[u8]) -> Vec<Rendered> {
        let reg = shared_registry();
        // The path is spelled as text for the adapter, which asks about
        // extensions and not about bytes. `from_utf8_lossy` is enough here:
        // the count of non-UTF-8 bytes is a wire concern — how many bytes
        // were not UTF-8 — and this crate answers with bytes, not with a
        // wire.
        let as_text = String::from_utf8_lossy(path);
        if !reg.may_claim_for_path(&as_text) {
            return Vec::new();
        }
        // The file's own bytes are not searchable text, so they are read
        // here and handed to the adapter, which renders them into text.
        let Some(data) = read_for_adapter(path) else {
            // The file could not be read. An empty vector sends the content
            // path back to the plain path, which reports the file the way
            // it reports any file it cannot read — a cloud placeholder
            // among them.
            return Vec::new();
        };
        let mut found = Vec::new();
        rgx_core::process(reg, &as_text, &data, 0, &Ctx::default(), &mut found);
        // An archive holds more than one document: its members are
        // documents of their own, and each carries its own path —
        // `bundle.zip!inner/record.txt` — because that is the path a query
        // asks about.
        found
            .into_iter()
            .map(|f| Rendered {
                path: f.path.into_bytes(),
                bytes: f.bytes,
            })
            .collect()
    }
}

/// The registry, built once.
///
/// The registry is ten families of adapters, and building it again for
/// every file would cost more than the search it serves — the daemon keeps
/// one in a `OnceLock` for the same reason, and this is that `OnceLock`.
fn shared_registry() -> &'static Registry {
    static REGISTRY: OnceLock<Registry> = OnceLock::new();
    REGISTRY.get_or_init(registry)
}

/// Read a file for an adapter: the same policy the plain path uses, and no
/// more.
///
/// The size cap is the content engine's (`MAX_FILE`), and the cap on the
/// bytes read is four times it — the same pair the plain path reads with,
/// so a file the plain path would refuse is a file the adapter path
/// refuses too.
fn read_for_adapter(path: &[u8]) -> Option<Vec<u8>> {
    use std::io::Read;
    let file = content::open_regular(path)?;
    let mut buf = Vec::new();
    file.take(content::MAX_FILE * 4)
        .read_to_end(&mut buf)
        .ok()?;
    Some(buf)
}

#[cfg(test)]
mod tests {
    use super::*;
    use rgy_name_core::content::Grep;
    use rgy_name_core::name_query::GrepMode;

    /// The marker the PDFs' text layers hold and their bytes do not.
    const MARKER: &str = "upstream mother of all libs";

    /// A file of our own, in the temp folder, gone when the test ends.
    struct TempFile {
        path: std::path::PathBuf,
    }

    impl TempFile {
        /// Write `bytes` under `name`, in the temp folder.
        fn new(name: &str, bytes: &[u8]) -> TempFile {
            let mut path = std::env::temp_dir();
            // The process id keeps two runs of the suite apart, and the
            // name keeps two files of one run apart.
            path.push(format!("rgy-adapters-{}-{name}", std::process::id()));
            std::fs::write(&path, bytes).expect("the temp file is written");
            TempFile { path }
        }

        /// The path, as the bytes the tree speaks.
        fn bytes(&self) -> Vec<u8> {
            self.path.to_string_lossy().into_owned().into_bytes()
        }
    }

    impl Drop for TempFile {
        fn drop(&mut self) {
            let _ = std::fs::remove_file(&self.path);
        }
    }

    /// A PDF whose text layer holds `marker` and whose bytes do not.
    ///
    /// The content stream is `/ASCIIHexDecode`d, so the words are in the
    /// file only as hex digits: a search over the raw bytes cannot find
    /// them, and the adapter is the only way to them.
    ///
    /// The file is built here and not read from upstream's fixture tree on
    /// purpose. A test that reaches into another crate's directory is a
    /// test that stops being a test the day that directory moves, and this
    /// one carries its own premise with it: the assertion that the bytes do
    /// not hold the marker is checked against the very bytes the search
    /// reads. The upstream fixture is read too, by
    /// `the_real_pdf_reads_through_the_same_path`, so the real thing is
    /// covered as well.
    fn pdf_with(marker: &str) -> Vec<u8> {
        // The content stream: a font, a text matrix, and the words.
        let content =
            format!("BT /F1 12 Tf 14.4 TL ET\nBT 1 0 0 1 100 750 Tm ({marker}) Tj T* ET\n");
        // The hex encoding of it, which is what the file carries.
        let mut hex = String::with_capacity(content.len() * 2 + 1);
        for b in content.bytes() {
            hex.push_str(&format!("{b:02X}"));
        }
        hex.push('>');

        let mut out: Vec<u8> = Vec::new();
        let mut offsets: Vec<usize> = Vec::new();
        let object = |out: &mut Vec<u8>, offsets: &mut Vec<usize>, body: &[u8]| {
            offsets.push(out.len());
            out.extend_from_slice(body);
        };

        out.extend_from_slice(b"%PDF-1.4\n");
        // 1: the page's font resource.
        object(
            &mut out,
            &mut offsets,
            b"1 0 obj\n<< /F1 2 0 R >>\nendobj\n",
        );
        // 2: the font, with the encoding that maps the codes to letters.
        object(
            &mut out,
            &mut offsets,
            b"2 0 obj\n<< /BaseFont /Helvetica /Encoding /WinAnsiEncoding /Name /F1 /Subtype /Type1 /Type /Font >>\nendobj\n",
        );
        // 3: the page.
        object(
            &mut out,
            &mut offsets,
            b"3 0 obj\n<< /Contents 4 0 R /MediaBox [ 0 0 595 842 ] /Parent 5 0 R /Resources << /Font 1 0 R >> /Type /Page >>\nendobj\n",
        );
        // 4: the content stream, hex-encoded.
        offsets.push(out.len());
        out.extend_from_slice(
            format!(
                "4 0 obj\n<< /Filter /ASCIIHexDecode /Length {} >>\nstream\n",
                hex.len()
            )
            .as_bytes(),
        );
        out.extend_from_slice(hex.as_bytes());
        out.extend_from_slice(b"\nendstream\nendobj\n");
        // 5: the page tree.
        object(
            &mut out,
            &mut offsets,
            b"5 0 obj\n<< /Count 1 /Kids [ 3 0 R ] /Type /Pages >>\nendobj\n",
        );
        // 6: the catalog.
        object(
            &mut out,
            &mut offsets,
            b"6 0 obj\n<< /Pages 5 0 R /Type /Catalog >>\nendobj\n",
        );

        // The cross-reference table: one entry per object, at the offset
        // the object starts.
        let xref = out.len();
        out.extend_from_slice(format!("xref\n0 {}\n", offsets.len() + 1).as_bytes());
        out.extend_from_slice(b"0000000000 65535 f \n");
        for off in &offsets {
            out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
        }
        out.extend_from_slice(
            format!(
                "trailer\n<< /Size {} /Root 6 0 R >>\nstartxref\n{xref}\n%%EOF\n",
                offsets.len() + 1
            )
            .as_bytes(),
        );
        out
    }

    /// A grep for a literal word.
    fn grep(pattern: &str) -> Grep {
        Grep::new(pattern, GrepMode::Literal).expect("the pattern compiles")
    }

    /// The bytes of a file, as the bytes the tree speaks.
    fn bytes_of(path: &std::path::Path) -> Vec<u8> {
        path.to_string_lossy().into_owned().into_bytes()
    }

    #[test]
    fn the_adapter_path_reads_a_pdf_text_layer() {
        let bytes = pdf_with(MARKER);
        // The premise: the marker is not in the bytes. A search over the
        // raw file cannot find it, so a match can only come from the
        // adapter.
        assert!(
            !bytes.windows(MARKER.len()).any(|w| w == MARKER.as_bytes()),
            "the marker is not in the raw bytes"
        );
        let file = TempFile::new("adapter.pdf", &bytes);
        let path = file.bytes();

        let res = content::verify_paths_with(
            &grep(MARKER),
            std::slice::from_ref(&path),
            10,
            Some(&Adapters),
        );

        assert_eq!(res.files.len(), 1, "the PDF matched");
        assert_eq!(
            res.files[0].path, path,
            "the answer names the file that was read"
        );
        assert!(
            res.files[0]
                .lines
                .iter()
                .any(|(_, line)| line.contains(MARKER)),
            "the line carries the marker: {:?}",
            res.files[0].lines
        );
        assert_eq!(res.candidates, 1, "the PDF was the candidate");
        assert!(res.complete, "the search covered the candidate");
    }

    #[test]
    fn the_real_pdf_reads_through_the_same_path() {
        // Upstream's own fixture: a real PDF whose text layer holds the
        // words and whose bytes do not. It is read at run time and not with
        // `include_bytes!`, because a build-time include of a path outside
        // this package is a build that breaks where the package is
        // unpacked — and a test that cannot run is not a test.
        let path = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("..")
            .join("rgx-adapters-pdf")
            .join("fixtures")
            .join("reportlab.pdf");
        let bytes = std::fs::read(&path).unwrap_or_else(|e| panic!("{path:?}: {e}"));
        assert!(
            !bytes.windows(MARKER.len()).any(|w| w == MARKER.as_bytes()),
            "the marker is not in the raw bytes"
        );
        let file = TempFile::new("reportlab.pdf", &bytes);
        let path = file.bytes();

        let res = content::verify_paths_with(
            &grep(MARKER),
            std::slice::from_ref(&path),
            10,
            Some(&Adapters),
        );

        assert_eq!(res.files.len(), 1, "the PDF matched");
        assert!(
            res.files[0]
                .lines
                .iter()
                .any(|(_, line)| line.contains(MARKER)),
            "the line carries the marker: {:?}",
            res.files[0].lines
        );
    }

    #[test]
    fn a_file_no_adapter_claims_renders_to_nothing() {
        // The contract the content path relies on: an empty vector means
        // "no adapter claims this file", and the plain path reads its own
        // bytes then. A reader that answered with something for a `.txt`
        // would take the file away from the plain path.
        let file = TempFile::new("unclaimed.txt", b"a line\nthe marker is here\n");

        let rendered = Adapters.rendered(&file.bytes());

        assert!(
            rendered.is_empty(),
            "a plain text file is not rendered: {}",
            rendered.len()
        );
    }

    #[test]
    fn the_plain_path_reads_the_bytes() {
        // With the reader present, a file no adapter claims still reaches
        // the plain path, and is matched as bytes.
        let file = TempFile::new("plain.txt", b"a line\nthe marker is here\n");
        let path = file.bytes();

        let res = content::verify_paths_with(
            &grep("the marker"),
            std::slice::from_ref(&path),
            10,
            Some(&Adapters),
        );

        assert_eq!(res.files.len(), 1, "the text file matched");
        assert_eq!(res.files[0].path, path, "the answer names the file");
        assert!(
            res.files[0]
                .lines
                .iter()
                .any(|(_, line)| line.contains("the marker")),
            "the line carries the marker: {:?}",
            res.files[0].lines
        );
        assert_eq!(res.candidates, 1, "the text file was the candidate");
        assert!(res.complete, "the search covered the candidate");
    }

    #[test]
    fn the_limit_caps_the_answer() {
        // Two files that both match, and a limit of one: the answer carries
        // one file. The candidate it did not read was not needed — the
        // content path stopped because it had enough answers — and the
        // accounting of that is the content path's, not this crate's.
        let one = TempFile::new("limit-a.txt", b"the marker is here\n");
        let two = TempFile::new("limit-b.txt", b"the marker is here too\n");
        let paths = vec![one.bytes(), two.bytes()];

        let res = content::verify_paths_with(&grep("the marker"), &paths, 1, Some(&Adapters));

        assert_eq!(res.files.len(), 1, "the limit held");
        assert_eq!(res.candidates, 2, "both files were candidates");
    }

    #[test]
    fn the_rendering_carries_the_path_of_the_document() {
        // An adapter renders a file into a document, and the document
        // carries the path a query asks about: the file itself for a PDF,
        // `archive.zip!member` for a member of an archive.
        let file = TempFile::new("rendered.pdf", &pdf_with(MARKER));

        let rendered = Adapters.rendered(&file.bytes());

        assert!(!rendered.is_empty(), "the PDF rendered");
        assert!(
            rendered
                .iter()
                .any(|d| d.path == file.bytes()
                    && String::from_utf8_lossy(&d.bytes).contains(MARKER)),
            "the rendering carries the path and the words"
        );
    }

    #[test]
    fn a_missing_file_renders_to_nothing() {
        // A file that is not there is not rendered, and the plain path
        // reports it as unreadable — the reader never invents a document.
        let path = bytes_of(std::path::Path::new(
            &std::env::temp_dir().join("rgy-adapters-not-there.pdf"),
        ));

        assert!(Adapters.rendered(&path).is_empty());
    }
}
