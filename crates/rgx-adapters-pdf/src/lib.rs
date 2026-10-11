//! In-process PDF text extraction — full version, copied from upstream
//! (`io::binary::pdf`, the mother of all libs).
//!
//! Scope: classic xref tables, xref streams (PDF 1.5+), object streams
//! (`ObjStm`), the full filter zoo (Flate, LZW, ASCII85, ASCIIHex,
//! RunLength, PNG predictors), page-tree walking with inherited resources,
//! and font-aware text extraction — simple-font encodings
//! (WinAnsi/MacRoman/Differences) and CID fonts via `ToUnicode` CMaps.
//!
//! Encrypted PDFs are detected and reported as a bail (rgx has no
//! decryption; such files simply yield no text).

#![no_std]

#[macro_use]
extern crate alloc;

#[cfg(test)]
extern crate std;

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::fmt::Write as _;
use rgx_core::Result;
use rgx_core::{Adapter, Outcome};

mod pdf_full;

use pdf_full::PdfReader;

pub fn adapters() -> Vec<Box<dyn Adapter>> {
    vec![Box::new(Pdf)]
}

pub struct Pdf;

impl Adapter for Pdf {
    fn name(&self) -> &'static str {
        "pdf"
    }
    fn extensions(&self) -> &'static [&'static str] {
        &["pdf"]
    }
    fn extract(&self, data: &[u8], _path: &str) -> Result<Outcome> {
        match PdfReader::parse(data) {
            Ok(doc) => {
                let extracted = doc.extract_text_with_notes();
                let note = undecodable_note(&extracted);
                let text = extracted.text;
                if text.trim().is_empty() && note.is_none() {
                    // No page text, and no font to explain why. Bail, so the
                    // pipeline falls through to the raw-byte passthrough,
                    // which for a scanned or image-only PDF is strictly more
                    // searchable than the few metadata fields would be — it
                    // reaches strings inside every object stream. Returning
                    // metadata here would *lose* that.
                    Ok(Outcome::Bail("pdf: no extractable text".into()))
                } else {
                    // Document metadata is real searchable text, so emit it
                    // ahead of the page text. Values are flattened to a single
                    // line: a PDF string may contain raw newlines, which would
                    // otherwise inject forged lines into the corpus —
                    // including lines shaped like the "Page N:" markers,
                    // which downstream tooling parses to attribute a match to
                    // a page.
                    let mut out = String::new();
                    for (key, value) in doc.metadata() {
                        let flat = value.split_whitespace().collect::<Vec<_>>().join(" ");
                        if !flat.is_empty() {
                            let _ = writeln!(out, "{key}: {flat}");
                        }
                    }
                    out.push_str(&text);
                    if let Some(note) = note {
                        out.push_str(&note);
                    }
                    Ok(Outcome::Text(out.into_bytes()))
                }
            }
            Err(pdf_full::PdfError::Encrypted) => Ok(Outcome::Bail("pdf: encrypted".into())),
            Err(_) => Ok(Outcome::Bail("pdf: unparseable".into())),
        }
    }
}

/// One searchable line saying which fonts left text out of the extraction
/// and why, so a file that is only partly readable is accounted for instead
/// of quietly answering with the part that happened to decode.
///
/// The alternative this replaces read an unmapped code as Latin-1, which for
/// `tesla-valve.pdf`'s Type 3 fonts turned glyph `/a97` into the letter
/// `a` — plausible, and wrong, in general. A wrong line is a wrong search
/// result; a missing line is a visible gap. The same rule is why
/// `rgx_core::process` keeps a file's own bytes beside an adapter failure
/// note rather than in place of it.
///
/// No note is emitted when nothing was left out — the control case (a
/// TrueType subset with a `/ToUnicode` CMap) must not grow a line.
fn undecodable_note(e: &pdf_full::Extraction) -> Option<String> {
    if !e.is_partial() {
        return None;
    }
    let fonts: usize = e.undecodable.iter().map(|(_, n)| n).sum();
    let reasons = e
        .undecodable
        .iter()
        .map(|(why, _)| *why)
        .collect::<Vec<_>>()
        .join(" / ");
    let tail = if e.missing_fonts > 0 {
        format!(
            "; {} content-stream font name(s) were absent from /Resources",
            e.missing_fonts
        )
    } else {
        String::new()
    };
    Some(format!(
        "[rgx: pdf: {fonts} font(s) over {} page(s) had no decodable character encoding \
         ({reasons}){tail}; their glyphs are absent from the text above]\n",
        e.pages
    ))
}

#[cfg(test)]
mod tests {
    //! Fixture tests: what each real and each constructed PDF has to yield.
    //!
    //! The failure these would miss is the silent kind. Every assertion is on
    //! exact characters or on the note, because a reader that answers with the
    //! wrong letter still returns a `String` and still looks like it works.
    //!
    //! `pdflatex-cm.pdf` and `lualatex-truetype.pdf` are real output. They were
    //! built with, and can be rebuilt by:
    //!
    //! ```text
    //! pdflatex  --disable-installer -interaction=nonstopmode cm.tex
    //! lualatex  --disable-installer -interaction=nonstopmode tt.tex
    //! ```
    //!
    //! where `cm.tex` is `\documentclass{article}\pagestyle{empty}\begin{document}
    //! \noindent Valvular calculus, please.\end{document}` (CM Type 1 subsets)
    //! and `tt.tex` is the same with `\usepackage{fontspec}` and
    //! `\setmainfont{Verdana}` over `C:/Windows/Fonts/verdana.ttf` (a Type 0 /
    //! CIDFontType2 TrueType subset). The two date strings and the `/ID` were
    //! then rewritten in place to fixed values of the same length, so a rebuild
    //! is byte-identical. `pdftotext` agrees with every assertion below.
    //!
    //! The other four are written by `scripts/make-pdf-fixtures.py`, because
    //! they are shapes no producer emits: a `/Type1` font that is not embedded
    //! and has nothing but a base encoding to go on, a `/Differences` array of
    //! glyph names that carry no Unicode, and a symbolic font with no encoding
    //! at all.
    use super::*;

    fn extract(data: &[u8]) -> Outcome {
        Pdf.extract(data, "t.pdf").unwrap()
    }

    fn text_of(data: &[u8]) -> String {
        let Outcome::Text(text) = extract(data) else {
            panic!("expected text, got a bail");
        };
        String::from_utf8(text).expect("adapter text is utf-8")
    }

    /// A classic-xref PDF holding the objects it is given, in order.
    struct Mini {
        buf: Vec<u8>,
        offsets: Vec<usize>,
    }

    impl Mini {
        fn new() -> Self {
            Self {
                buf: b"%PDF-1.4\n".to_vec(),
                offsets: Vec::new(),
            }
        }
        fn obj(&mut self, num: u32, body: &str) {
            self.offsets.push(self.buf.len());
            self.buf
                .extend_from_slice(format!("{num} 0 obj\n{body}\nendobj\n").as_bytes());
        }
        fn stream(&mut self, num: u32, data: &[u8]) {
            self.offsets.push(self.buf.len());
            self.buf.extend_from_slice(
                format!("{num} 0 obj\n<< /Length {} >>\nstream\n", data.len()).as_bytes(),
            );
            self.buf.extend_from_slice(data);
            self.buf.extend_from_slice(b"\nendstream\nendobj\n");
        }
        fn finish(self, size: u32) -> Vec<u8> {
            let mut out = self.buf;
            let xref = out.len();
            out.extend_from_slice(format!("xref\n0 {size}\n").as_bytes());
            out.extend_from_slice(b"0000000000 65535 f \n");
            for off in &self.offsets {
                out.extend_from_slice(format!("{off:010} 00000 n \n").as_bytes());
            }
            out.extend_from_slice(
                format!("trailer\n<< /Size {size} /Root 1 0 R >>\nstartxref\n{xref}\n%%EOF\n")
                    .as_bytes(),
            );
            out
        }
    }

    #[test]
    fn reportlab_real_file() {
        let text = text_of(include_bytes!("../fixtures/reportlab.pdf"));
        assert!(text.contains("Page 1:"), "{text}");
        assert!(text.contains("upstream mother of all libs"), "{text}");
        assert!(text.contains("segunda linha zebrada"), "{text}");
    }

    #[test]
    fn a_fully_decodable_document_grows_no_note() {
        // The note is only worth anything if it is rare. A base-14 Type 1
        // with `/Encoding /WinAnsiEncoding` and no `/ToUnicode` decodes
        // completely, so a `[rgx:` line here would be a lie.
        let text = text_of(include_bytes!("../fixtures/reportlab.pdf"));
        assert!(!text.contains("[rgx:"), "{text}");
    }

    #[test]
    fn a_type1_font_with_a_base_encoding_and_no_tounicode_yields_its_text() {
        let text = text_of(include_bytes!("../fixtures/type1-base-encoding.pdf"));
        assert!(text.contains("Valve the Tesla regulator"), "{text}");
        assert!(!text.contains("[rgx:"), "{text}");
    }

    #[test]
    fn a_differences_array_wins_over_the_base_encoding_beneath_it() {
        // 0x27 is `quoteright` and 0x60 is `quoteleft` under
        // StandardEncoding; 0xB1 and 0xD0 are `endash` and `emdash` and
        // come from the base encoding itself. Latin-1 would answer the
        // first two wrong and drop the last two. `pdftotext` reads this
        // file as U+2019 U+2018 U+2013 U+2014.
        let text = text_of(include_bytes!("../fixtures/type1-differences.pdf"));
        assert!(
            text.contains("Va\u{2019}lve\u{2019}s \u{2018}quoted\u{2018} \u{2013}em\u{2014}dash"),
            "{text:?}"
        );
        assert!(!text.contains('\''), "{text:?}");
        assert!(!text.contains('`'), "{text:?}");
        assert!(!text.contains("[rgx:"), "{text}");
    }

    #[test]
    fn a_pdflatex_document_reads_end_to_end() {
        // The producer of this crate's own flagship document, at 1/400th
        // the size: Type 1 subsets, `/ToUnicode` plus `/Encoding`, and an
        // indirect `/Resources`. Before the indirect-`/Resources` and the
        // `TJ`-array fixes this file extracted to nothing at all.
        let text = text_of(include_bytes!("../fixtures/pdflatex-cm.pdf"));
        assert!(text.contains("Valvular calculus, please."), "{text}");
        assert!(!text.contains("[rgx:"), "{text}");
    }

    #[test]
    fn a_truetype_subset_with_a_tounicode_cmap_reads_without_a_note() {
        // Type 0 / CIDFontType2, so the two-byte path: every code is a CID
        // and only the CMap can place it.
        let text = text_of(include_bytes!("../fixtures/lualatex-truetype.pdf"));
        assert!(text.contains("Valvular calculus, please."), "{text}");
        assert!(!text.contains("[rgx:"), "{text}");
    }

    #[test]
    fn an_opaque_type3_glyph_name_is_reported_rather_than_read_as_a_letter() {
        // The page also has a base-14 font, so the test has to see both
        // halves: the decodable text survives, the note names the gap, and
        // the two opaque codes 0x61/0x62 did not become the letters `a`
        // and `b`. Only the page text is searched for the letters — the
        // note contains the word "absent".
        let text = text_of(include_bytes!("../fixtures/type3-opaque-glyph-names.pdf"));
        let (page, note) = text.split_at(text.find("[rgx:").expect("a note"));
        assert!(page.contains("Valve the Tesla regulator"), "{page:?}");
        // A Latin-1 fallback would have made the two opaque codes the
        // letters `a` and `b`; the codes around them (0x51, 0x52) are not
        // in the font's /Differences at all, so nothing renders for them.
        assert!(!page.contains("QabR"), "{page:?}");
        assert!(!page.contains("ab"), "{page:?}");
        assert!(
            note.starts_with(
                "[rgx: pdf: 1 font(s) over 1 page(s) had no decodable character encoding"
            ),
            "{note}"
        );
        assert!(
            note.contains("/Differences names only glyphs with no Unicode"),
            "{note}"
        );
    }

    #[test]
    fn a_symbolic_font_with_no_encoding_reports_instead_of_bailing_to_raw_bytes() {
        // Every code in this document is undecodable, so the page text is
        // empty. The file must still answer with a searchable note: a
        // `Bail` here would hand the file back as its own compressed
        // bytes, which is the vanishing this crate's contract forbids.
        let text = text_of(include_bytes!("../fixtures/type1-symbolic-no-encoding.pdf"));
        assert!(
            text.contains("symbolic font whose encoding lives in the font program"),
            "{text}"
        );
        assert!(!text.contains("Valve"), "{text}");
    }

    #[test]
    fn a_scanned_pdf_with_no_fonts_at_all_still_bails_to_raw_bytes() {
        // The other half of the rule: with nothing to report, an empty page
        // must still bail, because for an image-only PDF the file's own
        // bytes are more searchable than its metadata would be.
        let mut b = Mini::new();
        b.obj(1, "<< /Type /Catalog /Pages 2 0 R >>");
        b.obj(2, "<< /Type /Pages /Kids [3 0 R] /Count 1 >>");
        b.obj(
            3,
            "<< /Type /Page /Parent 2 0 R /Resources << >> /Contents 4 0 R >>",
        );
        b.stream(4, b"q 595 0 0 842 0 0 cm /Im0 Do Q");
        let pdf = b.finish(5);
        assert!(matches!(
            Pdf.extract(&pdf, "scan.pdf").unwrap(),
            Outcome::Bail(_)
        ));
    }

    #[test]
    fn non_pdf_bails() {
        assert!(matches!(
            Pdf.extract(b"not a pdf at all", "t.pdf").unwrap(),
            Outcome::Bail(_)
        ));
        assert!(matches!(
            Pdf.extract(b"", "t.pdf").unwrap(),
            Outcome::Bail(_)
        ));
    }

    #[test]
    fn never_panics_on_mutation() {
        rgx_core::fuzzkit::assert_never_panics(
            vec![
                (
                    "reportlab",
                    include_bytes!("../fixtures/reportlab.pdf").to_vec(),
                ),
                (
                    "pdflatex",
                    include_bytes!("../fixtures/pdflatex-cm.pdf").to_vec(),
                ),
                (
                    "truetype",
                    include_bytes!("../fixtures/lualatex-truetype.pdf").to_vec(),
                ),
                (
                    "type3",
                    include_bytes!("../fixtures/type3-opaque-glyph-names.pdf").to_vec(),
                ),
                ("magic-only", b"%PDF-1.4\n".to_vec()),
                ("empty", vec![]),
            ],
            2000,
            "pdf",
            |data| {
                let _ = Pdf.extract(data, "fuzz.pdf");
            },
        );
    }
}
