// SPDX-License-Identifier: Apache-2.0

//! The rgx adapter registry: the formats a search can read *into*.
//!
//! A name search answers "which file is it". A content search answers
//! "what is inside it" — and for a PDF, a `.docx`, a zip, a mail file or a
//! 3D model, "inside" is a question only an adapter can answer. Those
//! adapters are upstream's, and they are vendored here; this crate is the
//! one thing they were missing: **the registry that orders them**.
//!
//! The registry is upstream's too (`rgx_core::Registry`), and the order
//! matters: the first matching available adapter wins, and a bail falls
//! through to the next. So the order here is the order upstream's own
//! binary uses — archives, office, 3D, llm, media, tabular, pdf, mail,
//! docs, inspect — and it is the order the families are declared in here.
//!
//! # Why it is a crate of its own
//!
//! The adapters are heavy and the daemon is not. A crate that builds the
//! registry is a crate that links ten adapter families, and the daemon
//! links it only when the second layer is asked for.
//!
//! # Why it is `std`
//!
//! The crate is `std`, and it is the one crate of this repository that the
//! bare-metal gate does not name. The reason is in the crates it links:
//! the ten adapter families are upstream's, and upstream builds them with
//! their formats turned on — and every one of those formats turns `std`
//! on with it, because a decoder that reads a file is a decoder with a
//! filesystem. A crate that orders them cannot be `no_std` while they are
//! not, and the vendored trees are not ours to change.
//!
//! The upstream tree has the same shape one crate over: `rgx-adapters-docs`
//! is `std` because it spawns `soffice` and `tesseract`, and it is the
//! one crate upstream's own bare-metal gate does not name.
//!
//! What the daemon gets is the whole registry — the ten families, in
//! upstream's order — because a search that reads a PDF and a search that
//! does not are not the same search.
//!
//! # The reader
//!
//! The registry is half of the answer; the other half is the reader that
//! uses it. [`Adapters`] is the registry as the content path's seam: it
//! answers "what does this file render to?", and the content path does the
//! reading, the accounting and the merge — so a `grep:` answers the same
//! question the same way whichever path it takes.

mod reader;

pub use reader::Adapters;

use rgx_core::{Adapter, Registry};

/// The registry, with every vendored family in it.
///
/// Building it costs a few hundred allocations and no I/O: the adapters
/// are cheap to construct, and the ones that need an external program
/// answer `available()` with whether it is there.
pub fn registry() -> Registry {
    let mut v: Vec<Box<dyn Adapter>> = Vec::new();
    v.extend(rgx_adapters_archive::adapters());
    v.extend(rgx_adapters_office::adapters());
    v.extend(rgx_adapters_threed::adapters());
    v.extend(rgx_adapters_llm::adapters());
    v.extend(rgx_adapters_media::adapters());
    v.extend(rgx_adapters_tabular::adapters());
    v.extend(rgx_adapters_pdf::adapters());
    v.extend(rgx_adapters_mail::adapters());
    v.extend(rgx_adapters_docs::adapters());
    v.extend(rgx_adapters_inspect::adapters());
    Registry::new(v)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_registry_has_the_families() {
        let reg = registry();
        // A PDF is an adapter's own, and a zip is a container: the two
        // answers the launcher cares about most.
        assert!(reg.may_claim_for_path("a.pdf"), "the pdf family is in");
        assert!(reg.may_claim_for_path("a.zip"), "the archive family is in");
    }

    #[test]
    fn a_plain_text_file_is_not_claimed() {
        // The plain path is the content engine's, and an adapter that
        // claimed a `.txt` would be an adapter replacing ripgrep for
        // nothing.
        let reg = registry();
        assert!(
            !reg.may_claim_for_path("a.txt"),
            "plain text is not claimed"
        );
    }
}
