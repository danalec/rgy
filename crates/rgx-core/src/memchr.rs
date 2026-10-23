// SPDX-License-Identifier: Apache-2.0

//! Re-export of the upstream `memchr` crate as an in-tree module.
//!
//! `memmem::Finder::find_iter` returns `Matches` whose
//! `Iterator::Item = (usize, usize)`. Bind it through `let (_, end) = ...`
//! and `let (start, _) = ...` so the 2.x rename of the tuple field is
//! invisible at the call site.

pub use ::memchr::*;
