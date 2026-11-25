// SPDX-License-Identifier: Apache-2.0

//! The incremental search session: what a keystroke re-uses.
//!
//! The engine keeps a session per search and re-uses the previous
//! result set when the query is edited, instead of walking the index again
//!. The win is the latency of a keystroke, and it is the
//! largest single win in the matrix.
//!
//! rgy's engine already re-uses one thing across a keystroke: the table of
//! scored **names** (`name_query::NameCache`). A query that narrows the
//! previous one re-scores the names the previous table holds, not the two
//! million it holds in all. What it does not have is a **session**: an
//! identity, a revision, and the previous result set held under both.
//!
//! This module is that session. It holds:
//!
//! * an **id**, so a client can keep its own session and the daemon can
//!   tell two of them apart (the reference's incremental-search id);
//! * the **revision** of the index the result set was found against, so a
//!   session whose index has moved is dropped rather than answered from;
//! * the **previous result set**, so the next keystroke re-ranks it
//!   instead of walking the index.
//!
//! ## What is re-ranked, and what is exact
//!
//! The re-rank is exact or it does not happen. A query may be answered
//! from the session only when it can match **no more** entries than the
//! previous one did -- the same rule the name table follows
//! ([`NameKey::narrows`]). Then the previous result set is a superset of
//! the new one, and re-ranking it gives the answer a fresh search gives.
//!
//! The previous result set has to be the **complete** match set for that to
//! hold, and a search that hit its cap does not have one: the entries the
//! cap dropped may be the ones the next keystroke wants. So a session
//! records whether its set is complete, and an incomplete one is not used
//! for a re-rank -- it is a fresh search, exactly as before.
//!
//! That is where the win comes from: a query that is close to its answer
//! has few hits, so its result set is complete, and the next keystroke
//! re-scores those few entries instead of the whole index. A query with
//! thousands of hits has an incomplete set, and the next keystroke walks
//! the index as before -- which is right, because the next keystroke would
//! have to look at all of them anyway.
//!
//! ## A refused query is refused here too
//!
//! A query the engine refuses -- an unknown `type:`, an uncompilable
//! `re:` -- matches nothing on every route: the fresh search fails closed
//! at its own door. The session fails closed the same way. It does not
//! search the index, and it does not answer from the result set it
//! holds: the refusal is a property of the query, not of the route that
//! answers it, and a session that answered a refused query with hits
//! would be a fail-open with the engine's own rule as the hole.
//!
//! The refusal is not part of the query's key, so a refused query can
//! carry the key of the query before it -- which is exactly why the
//! re-rank and the replay have to be told about it rather than found out
//! from the key.
//!
//! ## What the session does not change
//!
//! The answers are the answers a fresh search gives: the same [`Searcher`],
//! the same scorers, the same filters. A session that cannot re-rank
//! exactly is not used at all.

use crate::name_query::{Hit, NameKey, Query, Searcher, SortBy};
use alloc::string::{String, ToString};
use alloc::vec::Vec;

/// How much more a re-rank's visit costs than a step of the linear scan.
///
/// The scan reads the names in order, at about 0.3 ns an entry; a re-rank
/// goes to each entry's own name, at about 18 ns. The re-rank therefore
/// only pays while the previous set is at most a sixtieth of what the scan
/// would walk -- the same rule the engine's own `selective` plan follows.
const RERANK_VISIT_RATIO: u64 = 60;

/// A search session: an identity, a revision, and the last result set.
///
/// The session is cheap to make and cheap to drop; what it holds is not
/// large (the result set, capped by the query's own limit).
pub struct Session {
    /// The session's identity. The client picks it, or the daemon does.
    id: u64,
    last: crate::sync::Mutex<Option<Last>>,
    /// Whether the last search was answered from the session.
    reranked: core::sync::atomic::AtomicBool,
}

/// The state a session keeps between two keystrokes.
struct Last {
    /// When the session was filled, in wall-clock milliseconds.
    #[cfg_attr(not(feature = "std"), allow(dead_code))]
    at: u64,
    /// The revision of the index the result set was found against.
    revision: u64,
    /// The query's key: what a later query is compared against.
    key: SessionKey,
    /// The facet selection the query carried: the filters without the text.
    facets: Facets,
    /// The answer the search gave, kept for the facet replay (Q.4).
    answer: Option<Vec<Hit>>,
    /// The base entries the search matched, with the name they matched
    /// under.
    ///
    /// The name id is what makes the re-rank cheap: the score of an entry
    /// comes from its name's score, and the name table already holds it.
    entries: Vec<(u32, u32)>,
    /// Whether `entries` is the complete match set.
    ///
    /// A search that hit its cap does not have one, and a set that is not
    /// complete cannot be re-ranked: the entries the cap dropped may be the
    /// ones the next keystroke wants.
    complete: bool,
}

impl Session {
    /// A session with the given identity and nothing in it.
    pub fn new(id: u64) -> Session {
        Session { id, last: crate::sync::Mutex::new(None), reranked: core::sync::atomic::AtomicBool::new(false) }
    }

    /// The session's identity.
    pub fn id(&self) -> u64 {
        self.id
    }

    /// Drop what the session holds.
    pub fn clear(&self) {
        *self.last.lock().unwrap() = None;
    }

    /// Whether the session holds anything.
    pub fn is_empty(&self) -> bool {
        self.last.lock().unwrap().is_none()
    }

    /// The number of entries the session holds.
    pub fn len(&self) -> usize {
        self.last.lock().unwrap().as_ref().map_or(0, |l| l.entries.len())
    }

    /// The revision the session's result set was found against.
    pub fn revision(&self) -> Option<u64> {
        self.last.lock().unwrap().as_ref().map(|l| l.revision)
    }

    /// Whether the session's result set is the complete match set.
    ///
    /// A search that hit its cap did not find everything, so its set is
    /// not complete and cannot be re-ranked: the entries the cap dropped
    /// may be the ones the next keystroke wants.
    pub fn is_complete(&self) -> bool {
        self.last.lock().unwrap().as_ref().is_some_and(|l| l.complete)
    }

    /// Whether the last search was answered from the session.
    ///
    /// This is how a caller -- and a test -- tells a re-rank from a fresh
    /// search. The answers are the same either way; the work is not.
    pub fn last_reranked(&self) -> bool {
        self.reranked.load(core::sync::atomic::Ordering::Relaxed)
    }

    /// Search the query, re-using the session when it can.
    ///
    /// The session is filled with what the search matched, so the next
    /// keystroke can re-rank it. The answers are the same either way: the
    /// re-rank path is only taken when it is provably the same set.
    ///
    /// A query the engine refused answers nothing here too, the way it
    /// answers nothing on the fresh path.
    pub fn search(&self, searcher: &Searcher<'_>, q: &Query) -> Vec<Hit> {
        // Fail closed at the door, the way the fresh path does.
        //
        // The session is an optimisation, and an optimisation that answers
        // a refused query is a fail-open: the fresh path answers nothing
        // for a query with an unknown `type:` or an uncompilable `re:`,
        // and this path searched the index and answered with the tree. A
        // client that named a session got hits where a client that did
        // not got none, and a refusal the engine made is a refusal of the
        // query -- not of the route that answers it.
        if q.never {
            return self.fail_closed(searcher, q);
        }
        // The session's entries are only good for the index they were
        // matched against. A different revision is a different index, and
        // the entries mean something else now.
        let revision = searcher.live.revision;
        match self.rerank(searcher, q, revision) {
            Some(hits) => {
                self.reranked.store(true, core::sync::atomic::Ordering::Relaxed);
                self.fill(searcher, q, revision, &hits, true);
                hits
            }
            None => {
                self.reranked.store(false, core::sync::atomic::Ordering::Relaxed);
                let (base, over) = searcher.search_split(q);
                let complete = base.len() + over.len() < q.limit;
                let mut hits = base;
                hits.extend(over);
                // The answer is stored AFTER the sort and the cap: the
                // replay path hands it back as it is, so an answer kept
                // before the cap was the unsorted, over-long list of the
                // two halves -- a different order and a different length
                // from the one a fresh search gives.
                let hits = self.sort_and_cap(searcher, q, hits);
                self.fill(searcher, q, revision, &hits, complete);
                hits
            }
        }
    }

    /// The answer to a query the engine refused, and what the session
    /// keeps of it.
    ///
    /// The answer is the fresh path's own: the searcher is asked, and it
    /// refuses at its own door ([`Searcher::search`]). There is no second
    /// copy of the rule here to drift from the searcher's -- the session
    /// asks the engine, and the engine says no.
    ///
    /// The session keeps nothing. A refused query matches nothing, so a
    /// result set left behind is a result set a later query could be
    /// answered from, and the refused query admitted no entry at all. The
    /// next normal query is therefore answered fresh: the session gives
    /// up its optimisation rather than keep it on a query that never
    /// matched. The answer is the same either way -- that is the whole
    /// promise the session makes.
    fn fail_closed(&self, searcher: &Searcher<'_>, q: &Query) -> Vec<Hit> {
        self.clear();
        // The answer came from neither the session nor the index: it is
        // the refusal itself.
        self.reranked.store(false, core::sync::atomic::Ordering::Relaxed);
        searcher.search(q)
    }

    /// The hits of `q`, re-ranked from the session's entries.
    ///
    /// `None` when the session cannot answer the query: the query widens
    /// the previous one, the index moved, the previous set was not
    /// complete, the previous set was too big for the re-rank to pay, or
    /// the session is empty. The answer is then a fresh search, and the
    /// caller cannot tell the difference -- which is the point.
    fn rerank(&self, searcher: &Searcher<'_>, q: &Query, revision: u64) -> Option<Vec<Hit>> {
        let entries = {
            let last = self.last.lock().unwrap();
            let last = last.as_ref()?;
            if last.revision != revision || !last.complete {
                return None;
            }
            // The query has to narrow the previous one, and it has to be the
            // same shape of query: a filter that changed the answer cannot be
            // re-ranked from the entries the old filter chose.
            if !SessionKey::of(q).narrows(&last.key) {
                return None;
            }
            // The re-rank visits the entries one by one, and a visit costs about
            // sixty times what a step of the linear scan costs (the scan reads
            // the names in order; the re-rank goes to each entry's name). So
            // the re-rank only pays when the previous set is much smaller than
            // what a fresh scan would walk -- the same rule the engine's own
            // `selective` plan follows. Without it the session would be a
            // pessimisation on a wide query, and a session that makes the
            // search slower is not an optimisation.
            let (lo, hi) = searcher.scope_range(q).unwrap_or((1, searcher.live.base.n));
            if (last.entries.len() as u64).saturating_mul(RERANK_VISIT_RATIO) > (hi - lo) as u64 {
                return None;
            }
            last.entries.clone()
        };
        Some(searcher.rerank_entries(q, &entries))
    }

    /// Fill the session with what the search matched.
    fn fill(&self, searcher: &Searcher<'_>, q: &Query, revision: u64, hits: &[Hit], complete: bool) {
        let mut entries = Vec::with_capacity(hits.len());
        for h in hits {
            if h.idx == u32::MAX {
                // An overlay entry: it has no entry in the index, so the
                // session cannot re-rank it. The overlay is searched on
                // every keystroke anyway.
                continue;
            }
            let name = searcher.live.base.ent_name()[h.idx as usize];
            entries.push((h.idx, name));
        }
        *self.last.lock().unwrap() = Some(Last {
            at: crate::name_query::now_millis(),
            revision,
            key: SessionKey::of(q),
            facets: Facets::of(q),
            answer: Some(hits.to_vec()),
            entries,
            complete,
        });
    }

    /// The hits in the query's order, capped.
    fn sort_and_cap(&self, searcher: &Searcher<'_>, q: &Query, mut hits: Vec<Hit>) -> Vec<Hit> {
        searcher.sort_hits_public(&mut hits, q.sort);
        hits.truncate(q.limit);
        hits
    }

    /// Drop the session once searching has stopped: the result set is worth
    /// keeping only while the user is typing.
    ///
    /// The `std` build: the wall clock says whether it has stopped.
    #[cfg(feature = "std")]
    pub fn trim_if_idle(&self, idle: core::time::Duration) {
        let mut g = self.last.lock().unwrap();
        // `>=`, not `>`: "idle for at least this long" is what a zero
        // threshold asks for, and a millisecond clock cannot promise more
        // precision than that.
        if g.as_ref().is_some_and(|s| crate::name_query::now_millis().saturating_sub(s.at) >= idle.as_millis() as u64) {
            *g = None;
        }
    }

    /// Without a clock there is no "idle" to decide, so the session stays.
    #[cfg(not(feature = "std"))]
    pub fn trim_if_idle(&self, _idle: core::time::Duration) {}

    /// Whether the session has been idle for `idle`.
    ///
    /// The daemon asks this of every session it keeps and drops the ones
    /// that have not been used. A session with no result set is idle: it
    /// holds nothing, and the next query reads the index anyway.
    ///
    /// The `std` build: the wall clock says whether it has stopped.
    #[cfg(feature = "std")]
    pub fn idle_for(&self, idle: core::time::Duration) -> bool {
        let g = self.last.lock().unwrap();
        g.as_ref().is_none_or(|s| crate::name_query::now_millis().saturating_sub(s.at) >= idle.as_millis() as u64)
    }

    /// Without a clock there is no "idle" to decide, so no session is idle.
    #[cfg(not(feature = "std"))]
    pub fn idle_for(&self, _idle: core::time::Duration) -> bool {
        false
    }

    /// The facet selection: the part of the query that is not the text the
    /// user is typing.
    ///
    /// The reference's product crate keeps the facet selection in a state
    /// machine, and an unchanged selection does not trigger a re-search:
    /// "file search window facet selection unchanged; skipping re-search to
    /// keep the engine replay path". The facet state lives
    /// in the product, not in the engine -- and rgy's product is the TUI
    /// and the daemon, which is where this is.
    ///
    /// The selection is the query's filters: the extension, the kind, the
    /// folder, the size, the date, the regex, the globs. The text is not
    /// part of it: the text is what changes on every keystroke, and the
    /// selection is what the user clicks.
    pub fn facets(&self) -> Facets {
        self.last.lock().unwrap().as_ref().map_or(Facets::default(), |l| l.facets.clone())
    }

    /// Search the query, replaying the previous answer when the query has
    /// not changed at all.
    ///
    /// This is the facet replay path. The selection is
    /// compared with the one the session holds, and so is the text: a
    /// query that is the same query is answered from the previous answer,
    /// with no search at all.
    ///
    /// The answer is the previous one, which is why the session has to
    /// hold it: a replay that cannot return the previous answer is not a
    /// replay. And it is the *whole* query that has to be the same: a
    /// query whose text changed has a different answer, and replaying the
    /// old one would be a stale answer.
    ///
    /// A query the engine refused is never replayed: it matches nothing,
    /// and the previous answer is the answer of another query.
    pub fn search_replaying_facets(&self, searcher: &Searcher<'_>, q: &Query) -> Vec<Hit> {
        // The refusal is not part of the key, so a refused query can carry
        // the key of the query before it -- and the replay below would
        // hand back that query's answer for a query that matches nothing.
        if q.never {
            return self.fail_closed(searcher, q);
        }
        let key = SessionKey::of(q);
        // The whole query is the same and the index has not moved: the
        // previous answer is the answer.
        if let Some(last) = self.last.lock().unwrap().as_ref()
            && last.key == key
            && last.revision == searcher.live.revision
            && let Some(hits) = &last.answer
        {
            self.reranked.store(true, core::sync::atomic::Ordering::Relaxed);
            return hits.clone();
        }
        self.reranked.store(false, core::sync::atomic::Ordering::Relaxed);
        self.search(searcher, q)
    }
}

/// The facet selection of a query: the filters, without the text.
///
/// The text is what the user types; the selection is what they click. The
/// two are kept apart so a selection that has not changed can be told from
/// a query that has.
#[derive(Clone, PartialEq, Default, Debug)]
pub struct Facets {
    kind: Option<u8>,
    link: bool,
    apps: bool,
    exts: Vec<Vec<u8>>,
    scope: Option<Vec<u8>>,
    size: (u64, u64),
    size_set: bool,
    mtime: (u32, u32),
    name_re: Option<String>,
    path_re: Option<String>,
    globs: Vec<(bool, Vec<u8>)>,
}

impl Facets {
    /// The selection of `q`.
    pub fn of(q: &Query) -> Facets {
        Facets {
            kind: q.kind,
            link: q.link,
            apps: q.apps,
            exts: q.exts.clone(),
            scope: q.scope.clone(),
            size: q.size,
            size_set: q.size_set,
            mtime: q.mtime,
            name_re: q.name_re.as_ref().map(|r| r.pattern().to_string()),
            path_re: q.path_re.as_ref().map(|r| r.pattern().to_string()),
            globs: q.globs.clone(),
        }
    }

    /// Whether the selection is empty: a query with no filters at all.
    pub fn is_empty(&self) -> bool {
        self.kind.is_none()
            && !self.link
            && !self.apps
            && self.exts.is_empty()
            && self.scope.is_none()
            && !self.size_set
            && self.mtime == (0, u32::MAX)
            && self.name_re.is_none()
            && self.path_re.is_none()
            && self.globs.is_empty()
    }
}

/// What a session's previous query decided, and what a later query has to
/// match to be answered from it.
///
/// The name table's key ([`NameKey`]) says which **names** a query matches.
/// The session needs more: the filters decide which **entries** match, and
/// a filter that changed the answer cannot be re-ranked from the entries
/// the old filter chose.
#[derive(PartialEq)]
pub struct SessionKey {
    names: NameKey,
    /// The filters on the entries themselves.
    filters: Filters,
}

/// The filters a query puts on the entries.
#[derive(PartialEq)]
struct Filters {
    kind: Option<u8>,
    link: bool,
    apps: bool,
    size: (u64, u64),
    size_set: bool,
    mtime: (u32, u32),
    /// The whole-path glob patterns, with their negation.
    globs: Vec<(bool, Vec<u8>)>,
    /// The path regex, as its text: the compiled form is not comparable.
    path_re: Option<String>,
    /// The folder the query asks for (`in:`).
    ///
    /// The scope is a filter, and a query that changes it asks a different
    /// question: `invoice` and `invoice in:sub2` do not have the same
    /// answer, so the session must not hand the second one the answer of
    /// the first.
    scope: Option<Vec<u8>>,
    /// The order the hits come in (`sort:`).
    ///
    /// The order is part of the answer, and a query that changes it asks
    /// for the same hits in a different order.
    sort: SortBy,
    /// How many hits the query asks for (`limit:`).
    limit: usize,
}

impl SessionKey {
    /// The key of `q`.
    fn of(q: &Query) -> SessionKey {
        SessionKey {
            names: NameKey::of(q, None),
            filters: Filters {
                kind: q.kind,
                link: q.link,
                apps: q.apps,
                size: q.size,
                size_set: q.size_set,
                mtime: q.mtime,
                globs: q.globs.clone(),
                path_re: q.path_re.as_ref().map(|r| r.pattern().to_string()),
                scope: q.scope.clone(),
                sort: q.sort,
                limit: q.limit,
            },
        }
    }

    /// Can only match entries `prev` matched: the same filters, and the
    /// tokens narrowed the way the name table's key narrows them.
    fn narrows(&self, prev: &SessionKey) -> bool {
        self.filters == prev.filters && self.names.narrows(&prev.names)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::live::Live;
    use crate::name_index::Index;
    use alloc::format;
    use alloc::vec;
    use rgy_discovery::walk::{KIND_FILE, KIND_LINK, Listing, NONE, RawEnt};

    /// An index of `n` files whose names are `entry_000000.txt` and so on,
    /// plus a few that do not follow the pattern.
    fn live(n: usize) -> Live {
        let mut names = Vec::new();
        let mut ents = Vec::new();
        for i in 0..n {
            let name = format!("entry_{i:06}.txt");
            let off = names.len() as u32;
            names.extend_from_slice(name.as_bytes());
            ents.push(RawEnt { name_off: off, name_len: name.len() as u16, kind: KIND_FILE, size: 10, mtime: 1_700_000_000, child: NONE });
        }
        for name in ["invoice.pdf", "invoice.txt"] {
            let off = names.len() as u32;
            names.extend_from_slice(name.as_bytes());
            ents.push(RawEnt { name_off: off, name_len: name.len() as u16, kind: KIND_FILE, size: 10, mtime: 1_700_000_000, child: NONE });
        }
        let off = names.len() as u32;
        names.extend_from_slice(b"entry_link");
        ents.push(RawEnt { name_off: off, name_len: 10, kind: KIND_LINK, size: 10, mtime: 1_700_000_000, child: NONE });
        Live::new(Index::build(vec![Listing { id: 0, names, ents }], 0, 1_700_000_000, b"/"))
    }

    fn q(s: &str) -> Query {
        Query::parse(s, "").expect("parses")
    }

    /// The entries a search answers, by their index in the index.
    ///
    /// The index is the identity: the path is rebuilt from it, and a hit
    /// in the overlay has no index at all (it has `u32::MAX`).
    fn paths(hits: &[Hit]) -> Vec<u32> {
        let mut v: Vec<u32> = hits.iter().map(|h| h.idx).collect();
        v.sort_unstable();
        v
    }

    /// A re-ranked answer is the answer a fresh search gives.
    ///
    /// This is the whole property: the session is an optimisation, and an
    /// optimisation that changes the answer is a bug. The queries are the
    /// ones a keystroke produces -- one token growing, a filter added, a
    /// kind narrowed.
    #[test]
    fn a_reranked_answer_is_the_answer_a_fresh_search_gives() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(1);
        let queries = ["entry", "entry_0", "entry_00", "entry_000", "invoice", "invoice.p", "entry_00 ext:txt", "entry_00 type:link"];
        for w in queries {
            let fresh = searcher.search(&q(w));
            let reranked = session.search(&searcher, &q(w));
            assert_eq!(paths(&reranked), paths(&fresh), "the session changed the answer of {w:?}: {:?} vs {:?}", paths(&reranked), paths(&fresh));
        }
    }

    /// A session that widens the query is not used.
    ///
    /// Deleting a character widens the match: the previous match set is not
    /// a superset of the new one, so the entries it holds are not enough.
    /// The session has to notice and answer with a fresh search.
    #[test]
    fn a_widening_query_is_answered_fresh() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(1);
        session.search(&searcher, &q("entry_00"));
        let fresh = searcher.search(&q("entry_0"));
        let got = session.search(&searcher, &q("entry_0"));
        assert_eq!(paths(&got), paths(&fresh), "a widening query is answered fresh: {:?} vs {:?}", paths(&got), paths(&fresh));
    }

    /// A filter that changed the answer cannot be re-ranked from the
    /// entries the old filter chose.
    #[test]
    fn a_changed_filter_is_answered_fresh() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(1);
        session.search(&searcher, &q("entry"));
        let fresh = searcher.search(&q("entry ext:pdf"));
        let got = session.search(&searcher, &q("entry ext:pdf"));
        assert_eq!(paths(&got), paths(&fresh), "a new filter is answered fresh: {:?} vs {:?}", paths(&got), paths(&fresh));
    }

    /// A session holds nothing after a query that matched no entry, and
    /// nothing after a clear.
    #[test]
    fn a_session_can_be_cleared() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(7);
        assert_eq!(session.id(), 7, "the session keeps its identity");
        session.search(&searcher, &q("entry"));
        assert!(!session.is_empty(), "a search fills the session");
        assert!(!session.is_empty(), "the session holds the entries it matched");
        session.clear();
        assert!(session.is_empty(), "a cleared session holds nothing");
        let hits = session.search(&searcher, &q("entry"));
        assert!(!hits.is_empty(), "a cleared session still answers");
    }

    /// The re-rank is actually taken.
    ///
    /// A test that only compares the answers cannot tell a re-rank from a
    /// fresh search -- and a session that never re-ranks would pass every
    /// other test here while doing nothing. So the session says whether it
    /// re-ranked.
    #[test]
    fn a_narrowing_query_is_re_ranked_and_not_searched_again() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(1);
        // A query with few hits: its result set is complete, so the next
        // keystroke can be answered from it.
        session.search(&searcher, &q("invoice"));
        assert!(session.is_complete(), "a query with few hits has a complete result set");
        // The next keystroke narrows it, and is answered from the session.
        session.search(&searcher, &q("invoice.p"));
        assert!(session.last_reranked(), "the narrowing query was re-ranked");
    }

    /// A query with more hits than the cap has no complete result set, so
    /// the next keystroke cannot be answered from it.
    #[test]
    fn a_query_that_hit_its_cap_cannot_be_re_ranked() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(1);
        // A query with more hits than the limit.
        let mut wide = q("entry");
        wide.limit = 5;
        let hits = session.search(&searcher, &wide);
        assert_eq!(hits.len(), 5, "the cap took five");
        assert!(!session.is_complete(), "a capped search has no complete result set");
        // The next keystroke is a fresh search.
        session.search(&searcher, &q("entry_0"));
        assert!(!session.last_reranked(), "a capped query cannot be re-ranked");
    }

    /// An unchanged query replays the previous answer.
    ///
    /// This is the facet replay path: the reference's product
    /// crate skips the re-search when the selection has not changed, and
    /// the engine's replay path is what it replays.
    #[test]
    fn an_unchanged_query_replays_the_previous_answer() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(1);
        // A search with a filter on it.
        let first = session.search_replaying_facets(&searcher, &q("entry ext:txt"));
        assert!(!first.is_empty(), "the search answered");
        // The same query: the answer is replayed, with no search.
        let second = session.search_replaying_facets(&searcher, &q("entry ext:txt"));
        assert!(session.last_reranked(), "the query did not change, so the answer was replayed");
        assert_eq!(paths(&second), paths(&first), "the replay answers the same as the search");
    }

    /// A session is dropped when the overlay moves.
    ///
    /// The index's own event id is fixed when the index is built, so it
    /// cannot say that the overlay moved: the session held one, and a
    /// session answered before a change was answered from after it -- a
    /// stale answer, in a session whose whole point is the answer.
    #[test]
    fn a_session_is_dropped_when_the_overlay_moves() {
        let mut live = live(200);
        let session = Session::new(1);
        let query = q("entry");
        {
            let searcher = Searcher { live: &live };
            let first = session.search_replaying_facets(&searcher, &query);
            assert!(!first.is_empty(), "the search answers something");
        }
        // The overlay moves.
        live.apply_events(&[]);
        let searcher = Searcher { live: &live };
        let _after = session.search_replaying_facets(&searcher, &query);
        assert!(!session.last_reranked(), "the session was answered from a result set the overlay moved past");
    }

    /// A query whose text changed is searched again.
    ///
    /// The facet selection did not change -- the filters are the same --
    /// but the text did, and the text decides the answer. Replaying the
    /// previous answer here would be a stale answer.
    #[test]
    fn a_query_whose_text_changed_is_searched_again() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(1);
        session.search_replaying_facets(&searcher, &q("entry ext:txt"));
        // The same filters, a different text.
        let got = session.search_replaying_facets(&searcher, &q("invoice ext:txt"));
        assert!(!session.last_reranked(), "the text changed, so the search ran again");
        let fresh = searcher.search(&q("invoice ext:txt"));
        assert_eq!(paths(&got), paths(&fresh), "the answer is the fresh one: {:?} vs {:?}", paths(&got), paths(&fresh));
    }

    /// A changed facet selection is a new search.
    #[test]
    fn a_changed_facet_selection_is_searched_again() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(1);
        session.search_replaying_facets(&searcher, &q("entry ext:txt"));
        // The selection changed: the filter is a different one.
        session.search_replaying_facets(&searcher, &q("entry ext:pdf"));
        assert!(!session.last_reranked(), "the selection changed, so the search ran again");
    }

    /// A replayed answer is the answer a fresh search gives.
    ///
    /// The session stored the answer before the sort and the cap, so the
    /// replay handed back the two halves of the result set as they came:
    /// unsorted, and longer than the query asked for. The order is part of
    /// the answer, and so is the length.
    #[test]
    fn a_replayed_answer_is_the_answer_a_fresh_search_gives() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(1);
        let query = q("entry");
        let fresh = searcher.search(&query);
        let first = session.search(&searcher, &query);
        let replayed = session.search_replaying_facets(&searcher, &query);
        assert_eq!(first.len(), fresh.len(), "the session answers what a fresh search gives");
        assert_eq!(replayed.len(), fresh.len(), "the replay keeps the length of the answer");
        let order: Vec<u32> = fresh.iter().map(|h| h.idx).collect();
        let replay_order: Vec<u32> = replayed.iter().map(|h| h.idx).collect();
        assert_eq!(order, replay_order, "the replay keeps the order of the answer");
    }

    /// A changed scope is a new search.
    ///
    /// `entry` and `entry in:sub2` do not have the same answer, and the
    /// scope was not part of the session's key: the second query replayed
    /// the answer of the first, which is a stale answer.
    #[test]
    fn a_changed_scope_is_searched_again() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(1);
        session.search_replaying_facets(&searcher, &q("entry"));
        session.search_replaying_facets(&searcher, &q("entry in:sub2"));
        assert!(!session.last_reranked(), "the scope changed, so the search ran again");
    }

    /// A changed order is a new search: the order is part of the answer.
    #[test]
    fn a_changed_order_is_searched_again() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(1);
        session.search_replaying_facets(&searcher, &q("entry"));
        session.search_replaying_facets(&searcher, &q("entry sort:mtime"));
        assert!(!session.last_reranked(), "the order changed, so the search ran again");
    }

    /// A changed cap is a new search.
    #[test]
    fn a_changed_cap_is_searched_again() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(1);
        session.search_replaying_facets(&searcher, &q("entry"));
        session.search_replaying_facets(&searcher, &q("entry limit:5"));
        assert!(!session.last_reranked(), "the cap changed, so the search ran again");
    }

    /// The selection is the filters, without the text.
    #[test]
    fn the_selection_is_the_filters_and_not_the_text() {
        let a = Facets::of(&q("entry ext:txt"));
        let b = Facets::of(&q("invoice ext:txt"));
        assert_eq!(a, b, "the text is not part of the selection");
        let c = Facets::of(&q("entry ext:pdf"));
        assert_ne!(a, c, "a different filter is a different selection");
        assert!(!a.is_empty(), "the selection has a filter in it");
        assert!(Facets::of(&q("entry")).is_empty(), "a query with no filters has an empty selection");
    }

    /// A query that fails closed answers nothing on the session path,
    /// exactly as it answers nothing on the fresh path.
    ///
    /// The fresh path fails closed at the door, and the session path did
    /// not: it searched the index and answered with the tree. A client
    /// that named a session got hits where a client that did not got
    /// none, and the refusal is a property of the query -- not of the
    /// route that answers it.
    #[test]
    fn a_query_that_fails_closed_answers_nothing_on_the_session_path() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let bad = q("invoice type:banana");
        assert!(bad.never, "the query failed closed");
        // A session with nothing in it.
        let cold = Session::new(1);
        assert_eq!(paths(&cold.search(&searcher, &bad)), paths(&searcher.search(&bad)), "an empty session refuses the query too");
        // A session with a result set in it: the refusal has to hold
        // in spite of it.
        let session = Session::new(2);
        let first = session.search(&searcher, &q("invoice"));
        assert!(!first.is_empty(), "the session answered something");
        let got = session.search(&searcher, &bad);
        let fresh = searcher.search(&bad);
        assert!(fresh.is_empty(), "the fresh path answers nothing");
        assert_eq!(paths(&got), paths(&fresh), "the session path answers what the fresh path gives: {:?} vs {:?}", paths(&got), paths(&fresh));
    }

    /// A refused query is not answered from the previous result set.
    ///
    /// The re-rank answers from the session's entries without touching
    /// the index, and the refusal is a property of the query: a query the
    /// engine refuses matches nothing, and the previous result set is no
    /// exception to that. The refusal is not part of the key either, so
    /// the re-rank would have taken the refused query -- the key is the
    /// key of the query before it.
    #[test]
    fn a_refused_query_is_not_answered_from_the_previous_result_set() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(1);
        // A query with few hits: its result set is complete, so the next
        // keystroke is answered from it.
        session.search(&searcher, &q("invoice"));
        assert!(session.is_complete(), "the result set is complete");
        session.search(&searcher, &q("invoice.p"));
        assert!(session.last_reranked(), "the narrowing query was re-ranked");
        // The same query with a filter the engine refuses.
        let bad = q("invoice.p type:banana");
        assert!(bad.never, "the query failed closed");
        let got = session.search(&searcher, &bad);
        assert!(got.is_empty(), "a refused query is not answered from the previous result set");
        assert!(!session.last_reranked(), "the answer came from neither the session nor the index");
    }

    /// A refused query is not answered by a replay.
    ///
    /// The replay hands back the previous answer when the whole query is
    /// the same, and the refusal is not part of the query's key: the
    /// refused query carries the key of the query before it, so the
    /// replay would have handed back that query's answer.
    #[test]
    fn a_refused_query_is_not_answered_by_a_replay() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(1);
        let first = session.search_replaying_facets(&searcher, &q("invoice.p ext:pdf"));
        assert!(!first.is_empty(), "the search answered something");
        // The same query with a filter the engine refuses.
        let bad = q("invoice.p ext:pdf type:banana");
        assert!(bad.never, "the query failed closed");
        let got = session.search_replaying_facets(&searcher, &bad);
        let fresh = searcher.search(&bad);
        assert!(fresh.is_empty(), "the fresh path answers nothing");
        assert_eq!(paths(&got), paths(&fresh), "the session path answers what the fresh path gives: {:?} vs {:?}", paths(&got), paths(&fresh));
        assert!(got.is_empty(), "a refused query is not answered by a replay");
    }

    /// The session keeps nothing after a refused query, and the next
    /// normal query still answers.
    ///
    /// The refusal leaves nothing behind: a result set kept under a
    /// refused query is a result set a later query could be answered
    /// from, and the refused query admitted no entry at all. The next
    /// normal query is therefore answered fresh -- the same answer, and
    /// that is what the session promises.
    #[test]
    fn a_refused_query_leaves_the_session_empty_and_the_next_query_answers() {
        let live = live(200);
        let searcher = Searcher { live: &live };
        let session = Session::new(1);
        session.search(&searcher, &q("invoice"));
        assert!(!session.is_empty(), "the session holds its result set");
        session.search(&searcher, &q("invoice type:banana"));
        assert!(session.is_empty(), "a refused query leaves the session holding nothing");
        assert_eq!(session.len(), 0, "a refused query leaves no entries behind");
        let got = session.search(&searcher, &q("invoice"));
        let fresh = searcher.search(&q("invoice"));
        assert!(!got.is_empty(), "the session still answers");
        assert_eq!(paths(&got), paths(&fresh), "the answer is the fresh one: {:?} vs {:?}", paths(&got), paths(&fresh));
    }
}
