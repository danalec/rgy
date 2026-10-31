// SPDX-License-Identifier: Apache-2.0

//! Where the answers come from.
//!
//! The window and the terminal UI are the same program wearing different
//! clothes: a query box, a list, a selection. What they differ on is
//! *where the answer comes from*, and that is this trait.
//!
//! Two implementations, and they are the two that exist:
//!
//! * the **daemon** — the index is already warm in another process, and a
//!   keystroke is a re-rank of the previous answer rather than a walk;
//! * the **local index** — the tree is in this process, and the first
//!   answer is the walk.
//!
//! The trait is in the `no_std` crate because it is a shape, not a
//! machine: the implementations live where their machine is.

use alloc::string::String;
use alloc::vec::Vec;

/// The answer to one query.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Answer {
    /// The paths, in the engine's order. The ranking is the product and
    /// nothing here re-sorts it.
    pub paths: Vec<String>,
    /// For each path, the character offsets of the matched characters in
    /// its **name**. Empty when the answer could not say which characters
    /// matched — a query with more than one alternative does not say which
    /// alternative a hit came from, and a span that guessed would be a lie
    /// in the place the user looks first.
    pub spans: Vec<Vec<u32>>,
    /// For each path, the line under the name.
    ///
    /// Empty for the name layer — the folder is there already — and the
    /// matched line for the content layer, which is the thing the user is
    /// actually asking about.
    pub contexts: Vec<String>,
    /// For each path, whether it is a folder.
    ///
    /// The source knows and the path does not say: a folder's path is
    /// spelled like a file's. A row that shows a folder with a file's icon
    /// shows the wrong picture of the thing the user is about to open.
    pub folders: Vec<bool>,
}

impl Answer {
    /// An answer with no results.
    pub fn empty() -> Answer {
        Answer::default()
    }

    /// How many results there are.
    pub fn len(&self) -> usize {
        self.paths.len()
    }

    /// Whether there are no results.
    pub fn is_empty(&self) -> bool {
        self.paths.is_empty()
    }

    /// The span of the result at `i`.
    pub fn span(&self, i: usize) -> &[u32] {
        self.spans.get(i).map(|s| s.as_slice()).unwrap_or(&[])
    }

    /// The line under the name of the result at `i`.
    pub fn context(&self, i: usize) -> &str {
        self.contexts.get(i).map(|s| s.as_str()).unwrap_or("")
    }

    /// Whether the result at `i` is a folder.
    ///
    /// A source that does not say is a source with no folders in it.
    pub fn folder(&self, i: usize) -> bool {
        self.folders.get(i).copied().unwrap_or(false)
    }
}

/// Where the answers come from.
pub trait Source {
    /// The answer to `query`, at most `limit` results.
    ///
    /// An empty query is not a special case here: it means "everything",
    /// and the two implementations answer it the two ways they can — the
    /// daemon ranks the whole index, the local one walks it.
    fn search(&mut self, query: &str, limit: usize) -> Answer;

    /// How many entries the source holds, when it can say.
    ///
    /// `None` is "I do not know", and it is not zero: a source that
    /// cannot say how big its index is has no count to show, and a
    /// header that says "0" about an index of a thousand entries is a
    /// wrong fact — and a wrong fact is worse than no fact, because
    /// nobody reads a number looking for a lie.
    ///
    /// The default is `None`, so a source that cannot answer is not
    /// obliged to: the trait is a shape, and the shape lets a source
    /// keep quiet about what it does not know.
    ///
    /// The number is the source's own: a source that does not walk the
    /// tree cannot know how big an index it does not have, so it says
    /// what the machine that holds the index said.
    fn indexed(&self) -> Option<usize> {
        None
    }
}
