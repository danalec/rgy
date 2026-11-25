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
//!
//! **The parsers run in this process.** `rgx_core::process` below decodes
//! archives, PDFs, office documents and media from bytes a stranger may
//! have written, in the same address space as the search. Rust rules out
//! most memory bugs, but a decoder can still panic, loop or allocate
//! without bound on hostile input, and none of those need `unsafe`. The
//! in-process limits (a 4 MiB input cap, a nesting depth of four,
//! 4096 members per level, and an `OutputCap` on the compression decoders)
//! bound the damage but do not isolate it. `RGY_ADAPTERS=off` is the
//! escape hatch for an untrusted tree: it makes `rendered` answer with
//! nothing for every file, so the content path reads only the file's own
//! bytes and no parser is invoked at all. See README.md and MANUAL.md.

use rgx_core::{Ctx, Registry};
use rgy_name_core::content::{self, AdapterReader, Rendered};
use std::sync::OnceLock;

use crate::registry;

/// The environment switch that turns every in-process parser off.
pub const ADAPTERS_ENV: &str = "RGY_ADAPTERS";

/// Whether one value of `RGY_ADAPTERS` asks for the parsers to be off.
///
/// Pure so the answer can be tested without touching the process's
/// environment: a value that is not one of the known off words leaves the
/// adapters on, and an unset switch leaves them on too.
pub fn adapters_disabled_from(value: Option<&str>) -> bool {
    let Some(value) = value else { return false };
    let value = value.trim();
    ["off", "0", "false", "no", "disabled", "none"]
        .iter()
        .any(|off| value.eq_ignore_ascii_case(off))
}

/// The switch, read once for the life of the process.
fn adapters_disabled() -> bool {
    static DISABLED: OnceLock<bool> = OnceLock::new();
    *DISABLED.get_or_init(|| adapters_disabled_from(std::env::var(ADAPTERS_ENV).ok().as_deref()))
}

/// The format adapters, as a reader for the content path.
///
/// The content path asks per file and this is where the registry answers.
pub struct Adapters;

impl Adapters {
    /// `rendered`, with the switch answered by the caller.
    ///
    /// Kept separate so a test can hold the disabled answer without
    /// mutating the process environment, which the rest of the suite
    /// shares.
    fn rendered_when(&self, path: &[u8], disabled: bool) -> Vec<Rendered> {
        if disabled {
            // No registry, no file read, no `rgx_core::process`: the
            // content path's plain branch reads the file's own bytes.
            return Vec::new();
        }
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

impl AdapterReader for Adapters {
    /// The documents the adapters rendered for `path`.
    ///
    /// An empty vector means no adapter claims the file: the content path
    /// reads the file's own bytes then, exactly as it always did. With
    /// `RGY_ADAPTERS=off` that is the answer for every file.
    fn rendered(&self, path: &[u8]) -> Vec<Rendered> {
        self.rendered_when(path, adapters_disabled())
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

    // --- the untrusted-tree switch -----------------------------------------

    #[test]
    fn an_unset_switch_keeps_the_adapters_on() {
        assert!(!adapters_disabled_from(None), "unset means on");
        assert!(!adapters_disabled_from(Some("")), "empty means on");
        assert!(!adapters_disabled_from(Some("  \t ")), "blank means on");
    }

    #[test]
    fn the_switch_knows_its_off_words() {
        for value in ["off", "0", "false", "no", "disabled", "none"] {
            assert!(
                adapters_disabled_from(Some(value)),
                "{value} asks for the parsers to be off"
            );
            assert!(
                adapters_disabled_from(Some(&format!("  {}  ", value.to_uppercase()))),
                "{value} is case-insensitive and trimmed"
            );
        }
    }

    #[test]
    fn an_unknown_value_keeps_the_adapters_on() {
        for value in ["on", "1", "true", "render", "nonsense"] {
            assert!(
                !adapters_disabled_from(Some(value)),
                "{value} is not an off word: failing safe means staying on"
            );
        }
    }

    #[test]
    fn a_disabled_reader_does_not_parse_a_pdf() {
        // The same PDF the adapter path reads above. With the switch off
        // the reader answers with nothing BEFORE the registry is even
        // consulted, so no decoder sees the file.
        let file = TempFile::new("switch-off.pdf", &pdf_with(MARKER));

        assert!(
            !Adapters.rendered_when(&file.bytes(), false).is_empty(),
            "on: the PDF is rendered"
        );
        assert!(
            Adapters.rendered_when(&file.bytes(), true).is_empty(),
            "off: the PDF is not parsed at all"
        );
    }

    /// A reader that never parses, the shape the disabled answer has.
    struct PlainOnly;

    impl AdapterReader for PlainOnly {
        fn rendered(&self, _path: &[u8]) -> Vec<Rendered> {
            Vec::new()
        }
    }

    #[test]
    fn a_disabled_search_reads_only_the_files_own_bytes() {
        // The marker lives in the PDF's text layer and not in its bytes.
        // With the parsers on the file matches; with them off the plain
        // path reads the very same file and does not match. Name search is
        // untouched either way — only the content path loses the adapter.
        let file = TempFile::new("switch-search.pdf", &pdf_with(MARKER));
        let path = file.bytes();
        let paths = std::slice::from_ref(&path);
        let needle = grep(MARKER);

        let with_parsers = content::verify_paths_with(&needle, paths, 10, Some(&Adapters));
        assert_eq!(with_parsers.files.len(), 1, "the adapter found the words");

        let without_parsers = content::verify_paths_with(&needle, paths, 10, Some(&PlainOnly));
        assert_eq!(
            without_parsers.files.len(),
            0,
            "the raw bytes do not hold the marker, and no parser ran"
        );
        assert_eq!(
            without_parsers.candidates, 1,
            "the file was still considered, not skipped"
        );
    }
}
