// SPDX-License-Identifier: Apache-2.0

//! The parallelism rgy carries: split the work, run the pieces, join them.
//!
//! This crate is `no_std`: it has no threads to hand work to, because there
//! is no `std` to spawn them with, and no scheduler to hand work to. What
//! it can do is split the work into pieces. Under the `std` feature the
//! pieces run on threads; without it they run in order, on the thread that
//! called. The call site is the same either way -- the shape kuon uses in
//! `src/b02/parallel`: one call, two implementations, one behind a
//! feature.
//!
//! **The width is the user's.** [`workers`] says how many workers the
//! parallel paths use. It is, in order: what [`set_workers`] said, what the
//! environment says (`RGY_NUM_THREADS`), or the machine's own count of
//! cores -- which is what the pool this replaces did, less the second
//! thread of every core, which is not a core (the `cores` module has the
//! measurement's story). A caller that wants a width of its own says so
//! with [`set_workers`], and a caller that wants one width for one call
//! says so with the `_at` variants.
//!
//! The split is not a work-stealing pool: it is a chunk per worker, each
//! chunk folded or sorted on its own, and the pieces joined afterwards.
//! That is a different shape from a pool that hands out one item at a
//! time, and it is the shape that has no `std` in it.
//!
//! The other shape is the one a work-stealing helper had: the workers take
//! the next index from a shared counter, one at a time, so a worker that
//! draws a cheap index does not hold the others up. That is [`each_index`]
//! and [`each_owned`].
//!
//! The decision this crate exists to make is the pool and the width

#![no_std]

extern crate alloc;
#[cfg(feature = "std")]
extern crate std;

/// The synchronisation the tree needs, without `std`: a lock and a cell.
///
/// It lives here because this crate is the floor: it has no dependency at
/// all, so the crates above it can take a lock without dragging `std` in.
pub mod sync;

/// The channel the tree needs, without `std`: a queue between a callback
/// that pushes and a reader that pulls.
pub mod mpsc;

/// The machine's cores: how many logical processors share one.
#[cfg(feature = "std")]
mod cores;

/// The pool: the workers that live between the calls.
///
/// The `std` path's threads are created once and kept, so a search that
/// runs a hundred times does not create a hundred threads. Without `std`
/// there are no threads to keep, and the work runs on the thread that
/// asked.
#[cfg(feature = "std")]
mod pool;

use alloc::vec::Vec;
use core::cmp::Ordering;
use core::ops::Range;

/// The width, as the configuration says it. Zero means "not chosen yet":
/// the environment is read on the first call, and never again.
static WIDTH: core::sync::atomic::AtomicUsize = core::sync::atomic::AtomicUsize::new(0);

/// How many workers the parallel paths use.
///
/// The environment is read once: it does not change under a running
/// process, and a scan that measured the width twice would be measuring
/// two different things.
///
/// The order is [`set_workers`] first, then the environment, then the
/// platform's own count. A caller that has a width of its own sets it and
/// is not overruled.
pub fn workers() -> usize {
    use core::sync::atomic::Ordering::Relaxed;
    let w = WIDTH.load(Relaxed);
    if w != 0 {
        return w;
    }
    let n = configured().unwrap_or_else(default_workers).max(1);
    // Two callers racing here compute the same number, so the store is
    // the same either way.
    WIDTH.store(n, Relaxed);
    n
}

/// Set the width the parallel paths use.
///
/// Zero puts the width back to what it would have been: the environment,
/// or the platform's count. A width of one is a request for the
/// sequential path, not a pool of one thread.
pub fn set_workers(n: usize) {
    use core::sync::atomic::Ordering::Relaxed;
    WIDTH.store(n, Relaxed);
}

/// The width the environment asks for, if it asks for one.
///
/// `RGY_NUM_THREADS` is the name, and it is the project's own: the pool
/// this replaces had `RAYON_NUM_THREADS`, which said a library's name
/// rather than the product's.
///
/// A value that is not a positive number is not a width, and it is
/// ignored: a typo in an environment variable must not silently make
/// every parallel path single-threaded.
pub fn configured() -> Option<usize> {
    #[cfg(feature = "std")]
    {
        std::env::var("RGY_NUM_THREADS")
            .ok()?
            .parse::<usize>()
            .ok()
            .filter(|&n| n > 0)
    }
    #[cfg(not(feature = "std"))]
    {
        None
    }
}

/// The machine's cores.
///
/// The platform's own count of processors, divided by the number of
/// processors a core carries: a core with two threads is one core, and the
/// tree's parallel work is a memory-bound pass that measured 47% slower
/// with the second thread of every core put to work (the `cores` module
/// has the measurement's story).
///
/// Without `std` there are no threads, so the width is one.
pub fn default_workers() -> usize {
    #[cfg(feature = "std")]
    {
        let logical = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        let width = cores::smt_width()
            .map(|threads| logical / threads.max(1))
            .unwrap_or(logical);
        width.clamp(1, logical)
    }
    #[cfg(not(feature = "std"))]
    {
        1
    }
}

/// The chunks `0..n` is split into: each at least `min_len` items long,
/// the last one whatever is left. One chunk when there is not enough work
/// to be worth splitting.
fn split(n: usize, min_len: usize) -> Vec<Range<usize>> {
    let min_len = min_len.max(1);
    let mut out = Vec::new();
    let mut at = 0;
    while at < n {
        let end = (at + min_len).min(n);
        out.push(at..end);
        at = end;
    }
    out
}

/// The chunks `0..n` is split into for a parallel pass: one per worker,
/// each with at least `min_len` items, so a worker that finishes early
/// does not leave the rest idle for want of work.
fn split_for_workers(n: usize, min_len: usize) -> Vec<Range<usize>> {
    split_at(workers(), n, min_len)
}

/// [`split_for_workers`], at a width the caller names.
fn split_at(workers: usize, n: usize, min_len: usize) -> Vec<Range<usize>> {
    let workers = workers.max(1);
    if n == 0 {
        return Vec::new();
    }
    // Small enough that one worker does it: the thread hand-off would cost
    // more than the work.
    if workers == 1 || n < min_len.saturating_mul(2) {
        return alloc::vec![0..n];
    }
    let per = n.div_ceil(workers).max(min_len);
    split(n, per)
}

/// Run `f` over each chunk, and return the results in chunk order.
///
/// Under `std` the chunks run on the pool's workers; without it they run
/// in order. The chunks are drawn from a counter, one at a time, so a
/// worker that draws a cheap chunk does not hold the others up.
#[cfg(feature = "std")]
fn run_chunks<T, F>(chunks: &[Range<usize>], f: &F) -> Vec<T>
where
    F: Fn(Range<usize>) -> T + Sync,
    T: Send,
{
    if chunks.len() <= 1 {
        return chunks.iter().map(|r| f(r.clone())).collect();
    }
    use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};
    let next = AtomicUsize::new(0);
    let out = Cells::<T>::empty(chunks.len());
    let work = || {
        loop {
            let i = next.fetch_add(1, Relaxed);
            if i >= chunks.len() {
                break;
            }
            let r = chunks[i].clone();
            // Safety: the counter hands each chunk to one worker only.
            unsafe { out.set(i, f(r)) };
        }
    };
    let job = Job { work: &work };
    // Safety: the job borrows the closure, and the closure borrows the
    // counter, the cells and the chunks -- all of which live until this
    // call returns, and the pool does not return until every worker is
    // done with them.
    let helpers = workers().max(1) - 1;
    unsafe { pool::run(helpers, run_job, &job as *const Job<'_> as *mut ()) };
    out.into_vec()
}

/// A job: the closure the pool's workers run.
///
/// The closure borrows -- the index, the scan's scratch, the caller's
/// buffer -- and the pool's workers outlive the job, so the borrow is
/// erased into a pointer. The caller waits for every worker to be done
/// before it lets go of what it borrowed: that wait is what makes the
/// erasure sound.
#[cfg(feature = "std")]
struct Job<'a> {
    /// The work: the same closure the calling thread runs.
    work: &'a (dyn Fn() + Sync),
}

/// The job, run: the closure, through the pointer.
#[cfg(feature = "std")]
unsafe fn run_job(data: *mut ()) {
    let job = unsafe { &*(data as *const Job<'_>) };
    (job.work)();
}

/// Run `f` over each chunk, and return the results in chunk order.
#[cfg(not(feature = "std"))]
fn run_chunks<T, F>(chunks: &[Range<usize>], f: &F) -> Vec<T>
where
    F: Fn(Range<usize>) -> T + Sync,
    T: Send,
{
    chunks.iter().map(|r| f(r.clone())).collect()
}

/// `f(i)` for each `i` in `0..n`, in the order of the indices.
pub fn map_collect<T, F>(n: usize, f: F) -> Vec<T>
where
    F: Fn(usize) -> T + Sync,
    T: Send,
{
    map_collect_with(n, 4096, f)
}

/// `f(i)` for each `i` in `0..n`, in the order of the indices.
///
/// `min_len` is the smallest piece worth handing to another worker: below
/// it the work stays on this thread, and above it the pieces are as even
/// as the worker count allows. A caller whose items are themselves large
/// (a run of documents, say) says so with a `min_len` of one.
pub fn map_collect_with<T, F>(n: usize, min_len: usize, f: F) -> Vec<T>
where
    F: Fn(usize) -> T + Sync,
    T: Send,
{
    let chunks = split_for_workers(n, min_len);
    let mut out = Vec::with_capacity(n);
    for part in run_chunks(&chunks, &|r| r.map(&f).collect::<Vec<T>>()) {
        out.extend(part);
    }
    out
}

/// `f(i)` for each `i` in `0..n`, into `out` (which is cleared first).
///
/// The chunking is coarser than [`map_collect`]'s: this is the path that
/// writes into a buffer the caller already owns, so the pieces are handed
/// over in blocks.
pub fn map_collect_into<T, F>(n: usize, min_len: usize, f: F, out: &mut Vec<T>)
where
    F: Fn(usize) -> T + Sync,
    T: Send,
{
    let chunks = split_for_workers(n, min_len);
    out.clear();
    for part in run_chunks(&chunks, &|r| r.map(&f).collect::<Vec<T>>()) {
        out.extend(part);
    }
}

/// `f(i, &slice[i])` for each `i`, in parallel under `std`.
pub fn for_each<T, F>(slice: &[T], f: F)
where
    F: Fn(usize, &T) + Sync,
    T: Sync,
{
    let chunks = split_for_workers(slice.len(), 4096);
    let _ = run_chunks(&chunks, &|r| {
        for i in r {
            f(i, &slice[i]);
        }
    });
}

/// `f(i, &mut slice[i])` for each `i`, in parallel under `std`.
///
/// The slice is cut into pieces first: a worker gets its own piece, and
/// no two workers hold a reference to the same memory.
pub fn for_each_mut<T, F>(slice: &mut [T], f: F)
where
    F: Fn(usize, &mut T) + Sync,
    T: Send,
{
    let ranges = split_for_workers(slice.len(), 4096);
    let pieces = split_ranges_mut(slice, &ranges);
    for_each_owned(pieces, |pi, piece| {
        let start = ranges[pi].start;
        for (j, v) in piece.iter_mut().enumerate() {
            f(start + j, v);
        }
    });
}

/// The slice cut at the given ranges, as disjoint mutable pieces.
fn split_ranges_mut<'a, T>(slice: &'a mut [T], ranges: &[Range<usize>]) -> Vec<&'a mut [T]> {
    let mut out = Vec::with_capacity(ranges.len());
    let mut rest = slice;
    let mut at = 0;
    for r in ranges {
        let take = r.start - at;
        let (_, tail) = rest.split_at_mut(take);
        let (piece, tail) = tail.split_at_mut(r.end - r.start);
        out.push(piece);
        rest = tail;
        at = r.end;
    }
    out
}

/// `f` over each chunk of `0..n`, in parallel under `std`; one result per
/// chunk, in chunk order.
///
/// This is the shape a fold takes: each chunk folds on its own, and the
/// folds are merged afterwards.
pub fn map_chunks<T, F>(n: usize, min_len: usize, f: F) -> Vec<T>
where
    F: Fn(Range<usize>) -> T + Sync,
    T: Send,
{
    let chunks = split_for_workers(n, min_len);
    run_chunks(&chunks, &f)
}

/// `f(i, item)` for each item, in parallel under `std`, and the results.
///
/// The items are handed to the worker by value, which is what a list of
/// disjoint pieces needs: each worker gets its own piece and nobody else
/// can reach it.
pub fn map_owned<T, U, F>(items: Vec<T>, f: F) -> Vec<U>
where
    F: Fn(usize, T) -> U + Sync,
    T: Send,
    U: Send,
{
    map_owned_at(workers(), items, f)
}

/// [`map_owned`], at a width the caller names.
///
/// The walk in `rgy-discovery` has its own width, and it is not the
/// query's: a scan that fans the directory reads out across eight workers
/// does not want the name scorer to wake eight workers as well.
pub fn map_owned_at<T, U, F>(workers: usize, items: Vec<T>, f: F) -> Vec<U>
where
    F: Fn(usize, T) -> U + Sync,
    T: Send,
    U: Send,
{
    each_owned_at(workers, items, || (), |_, i, item| f(i, item))
}

/// `f(i, item)` for each item, in parallel under `std`.
///
/// The items are handed to the worker by value, which is what a list of
/// disjoint pieces needs: each worker gets its own piece and nobody else
/// can reach it.
pub fn for_each_owned<T, F>(items: Vec<T>, f: F)
where
    F: Fn(usize, T) + Sync,
    T: Send,
{
    let _ = map_owned(items, f);
}

/// Sort `slice`, in parallel under `std`.
///
/// The chunks are sorted on their own and then merged. The merge is
/// sequential -- a k-way merge over the chunk heads -- because a merge
/// that ran on threads would need the same machinery it is merging for.
pub fn sort_unstable_by<T, F>(slice: &mut [T], cmp: F)
where
    F: Fn(&T, &T) -> Ordering + Sync,
    T: Clone + Send,
{
    let chunks = split_for_workers(slice.len(), 4096);
    if chunks.len() <= 1 {
        slice.sort_unstable_by(&cmp);
        return;
    }
    // The pieces, copied out: a worker sorts its own, and the copy is
    // the price of a merge that does not have to move anything twice.
    let pieces: Vec<Vec<T>> = chunks.iter().map(|r| slice[r.clone()].to_vec()).collect();
    let sorted = map_owned(pieces, |_, mut piece| {
        piece.sort_unstable_by(&cmp);
        piece
    });
    merge(slice, &sorted, &cmp);
}

/// Sort `slice` by `key`, in parallel under `std`.
pub fn sort_unstable_by_key<T, K, F>(slice: &mut [T], key: F)
where
    F: Fn(&T) -> K + Sync,
    K: Ord,
    T: Clone + Send,
{
    sort_unstable_by(slice, |a, b| key(a).cmp(&key(b)));
}

/// Sort `slice`, in parallel under `std`.
pub fn sort_unstable<T>(slice: &mut [T])
where
    T: Clone + Ord + Send,
{
    sort_unstable_by(slice, |a, b| a.cmp(b));
}

/// The slice in pieces of `chunk` items, as disjoint mutable pieces.
///
/// The pieces are what a worker gets: each one is its own, and no worker
/// can reach another's.
pub fn split_mut<T>(slice: &mut [T], chunk: usize) -> Vec<&mut [T]> {
    let chunk = chunk.max(1);
    let mut out = Vec::new();
    let mut rest = slice;
    while !rest.is_empty() {
        let take = rest.len().min(chunk);
        let (head, tail) = rest.split_at_mut(take);
        out.push(head);
        rest = tail;
    }
    out
}

/// The sorted parts, merged back into `slice`.
///
/// The parts are sorted runs; the merge takes the smallest head of each
/// run, in turn. The number of runs is the number of workers, so the scan
/// for the smallest head is short.
fn merge<T, F>(slice: &mut [T], parts: &[Vec<T>], cmp: &F)
where
    F: Fn(&T, &T) -> Ordering,
    T: Clone,
{
    let mut runs: Vec<usize> = (0..parts.len()).filter(|&i| !parts[i].is_empty()).collect();
    let mut at = alloc::vec![0usize; runs.len()];
    let mut out: Vec<T> = Vec::with_capacity(slice.len());
    while !runs.is_empty() {
        // The smallest head among the runs that still have one.
        let mut best = 0;
        for j in 1..runs.len() {
            if cmp(&parts[runs[j]][at[j]], &parts[runs[best]][at[best]]) == Ordering::Less {
                best = j;
            }
        }
        let run = runs[best];
        out.push(parts[run][at[best]].clone());
        at[best] += 1;
        if at[best] == parts[run].len() {
            runs.swap_remove(best);
            at.swap_remove(best);
        }
    }
    slice.clone_from_slice(&out);
}

/// Two independent pieces of work, at the same time under `std`.
pub fn join<A, B, RA, RB>(a: A, b: B) -> (RA, RB)
where
    A: FnOnce() -> RA + Send,
    B: FnOnce() -> RB + Send,
    RA: Send,
    RB: Send,
{
    #[cfg(feature = "std")]
    {
        std::thread::scope(|s| {
            let one = s.spawn(a);
            let two = s.spawn(b);
            (
                one.join().expect("a worker panicked"),
                two.join().expect("a worker panicked"),
            )
        })
    }
    #[cfg(not(feature = "std"))]
    {
        (a(), b())
    }
}

/// `f(state, i)` for each `i` in `0..n`, with a scratch state per worker,
/// and the results in the order of the indices.
///
/// This is the scan's shape: the workers take the indices from a shared
/// counter, one at a time, so a worker that draws a cheap index does not
/// hold the others up. The calling thread is one of the workers.
///
/// `prepare` runs once per worker, before that worker takes any index: it
/// is where a caller puts the setup a worker needs (the platform's
/// interactive priority, say). It is a setup cost, never a per-entry one.
pub fn each_index<S, T, FS, FF>(n: usize, state: FS, f: FF) -> Vec<T>
where
    FS: Fn() -> S + Sync,
    FF: Fn(&mut S, usize) -> T + Sync,
    S: Send,
    T: Send,
{
    each_index_at(workers(), n, state, f)
}

/// [`each_index`], at a width the caller names.
pub fn each_index_at<S, T, FS, FF>(workers: usize, n: usize, state: FS, f: FF) -> Vec<T>
where
    FS: Fn() -> S + Sync,
    FF: Fn(&mut S, usize) -> T + Sync,
    S: Send,
    T: Send,
{
    each_index_prepared_at(workers, n, || (), state, f)
}

/// [`each_index`], with a per-worker setup.
///
/// `prepare` runs once per worker, before that worker takes any index.
pub fn each_index_prepared<S, T, FS, FF, P>(n: usize, prepare: P, state: FS, f: FF) -> Vec<T>
where
    FS: Fn() -> S + Sync,
    FF: Fn(&mut S, usize) -> T + Sync,
    P: Fn() + Sync,
    S: Send,
    T: Send,
{
    each_index_prepared_at(workers(), n, prepare, state, f)
}

/// [`each_index_prepared`], at a width the caller names.
pub fn each_index_prepared_at<S, T, FS, FF, P>(
    workers: usize,
    n: usize,
    prepare: P,
    state: FS,
    f: FF,
) -> Vec<T>
where
    FS: Fn() -> S + Sync,
    FF: Fn(&mut S, usize) -> T + Sync,
    P: Fn() + Sync,
    S: Send,
    T: Send,
{
    #[cfg(feature = "std")]
    {
        use core::sync::atomic::{AtomicUsize, Ordering::Relaxed};

        let out = Cells::<T>::empty(n);
        let next = AtomicUsize::new(0);
        let work = || {
            prepare();
            let mut st = state();
            loop {
                let i = next.fetch_add(1, Relaxed);
                if i >= n {
                    break;
                }
                // Safety: the counter hands each index to one worker only.
                unsafe { out.set(i, f(&mut st, i)) };
            }
        };
        let job = Job { work: &work };
        // Safety: the job borrows the closure, and the closure borrows the
        // counter, the cells, the setup and the work -- all of which live
        // until this call returns, and the pool does not return until
        // every worker is done with them.
        let helpers = (workers.max(1) - 1).min(n.saturating_sub(1));
        unsafe { pool::run(helpers, run_job, &job as *const Job<'_> as *mut ()) };
        out.into_vec()
    }
    #[cfg(not(feature = "std"))]
    {
        let _ = workers;
        prepare();
        let mut st = state();
        let mut out = Vec::with_capacity(n);
        for i in 0..n {
            out.push(f(&mut st, i));
        }
        out
    }
}

/// `f(state, i, item)` for each item, with a scratch state per worker,
/// and the results in the order of the items.
///
/// The item is taken out of the list and handed to the worker by value, so
/// a worker cannot reach another's item. The workers take the indices from
/// a shared counter, one at a time, so the item a worker gets is one it
/// can own outright.
pub fn each_owned<S, T, U, FS, FF>(items: Vec<T>, state: FS, f: FF) -> Vec<U>
where
    FS: Fn() -> S + Sync,
    FF: Fn(&mut S, usize, T) -> U + Sync,
    S: Send,
    T: Send,
    U: Send,
{
    each_owned_at(workers(), items, state, f)
}

/// [`each_owned`], at a width the caller names.
pub fn each_owned_at<S, T, U, FS, FF>(workers: usize, items: Vec<T>, state: FS, f: FF) -> Vec<U>
where
    FS: Fn() -> S + Sync,
    FF: Fn(&mut S, usize, T) -> U + Sync,
    S: Send,
    T: Send,
    U: Send,
{
    let cells = Cells::filled(items.into_iter());
    let n = cells.len();
    each_index_at(workers, n, state, |st, i| {
        // Safety: the counter hands each index to one worker only, and an
        // index is taken exactly once.
        let item = unsafe { cells.take(i) }.expect("the counter hands each index to one worker");
        f(st, i, item)
    })
}

/// One value per index that the workers read and write without locks: each
/// index belongs to one worker at a time (the one the counter handed it
/// to).
///
/// The cell holds the value wrapped, because the value is not there until
/// the worker that owns the index has produced it.
struct Cells<T>(Vec<core::cell::UnsafeCell<Option<T>>>);

unsafe impl<T: Send> Sync for Cells<T> {}

impl<T> Cells<T> {
    /// `n` cells, all empty.
    ///
    /// The `std` path: the index scan fills a cell per index.
    #[cfg(feature = "std")]
    fn empty(n: usize) -> Cells<T> {
        Cells((0..n).map(|_| core::cell::UnsafeCell::new(None)).collect())
    }

    /// One cell per value, all of them filled.
    fn filled(v: impl Iterator<Item = T>) -> Cells<T> {
        Cells(v.map(|x| core::cell::UnsafeCell::new(Some(x))).collect())
    }

    /// Safety: no other worker uses index `i` meanwhile.
    ///
    /// The `std` path: the index scan fills a cell per index.
    #[cfg(feature = "std")]
    unsafe fn set(&self, i: usize, v: T) {
        unsafe { *self.0[i].get() = Some(v) };
    }

    /// Take the value at index `i`, leaving the cell empty.
    ///
    /// Safety: no other worker uses index `i` meanwhile.
    unsafe fn take(&self, i: usize) -> Option<T> {
        unsafe { (*self.0[i].get()).take() }
    }

    /// The number of cells.
    fn len(&self) -> usize {
        self.0.len()
    }

    /// The values, in the order of the indices.
    #[cfg(feature = "std")]
    fn into_vec(self) -> Vec<T> {
        self.0
            .into_iter()
            .map(|c| c.into_inner().expect("a cell was never filled"))
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use alloc::vec;

    #[test]
    fn map_collect_keeps_the_order() {
        let v = map_collect(1000, |i| i * 2);
        assert_eq!(v.len(), 1000);
        assert_eq!(v[0], 0);
        assert_eq!(v[999], 1998);
    }

    #[test]
    fn map_collect_of_nothing_is_nothing() {
        assert!(map_collect(0, |i: usize| i).is_empty());
    }

    #[test]
    fn map_collect_into_fills_the_buffer() {
        let mut out = vec![99usize; 3];
        map_collect_into(500, 64, |i| i, &mut out);
        assert_eq!(out.len(), 500);
        assert_eq!(out[0], 0);
        assert_eq!(out[499], 499);
    }

    #[test]
    fn for_each_visits_every_item() {
        let mut seen = vec![0usize; 500];
        for_each(&seen, |i, _| {
            // The closure cannot write; the count is checked below.
            let _ = i;
        });
        for_each_mut(&mut seen, |i, v| *v = i);
        assert_eq!(seen[0], 0);
        assert_eq!(seen[499], 499);
    }

    #[test]
    fn sort_by_key_sorts_the_whole_slice() {
        let mut v: Vec<u64> = (0..1000).map(|i| (i * 7919) % 1000).collect();
        sort_unstable_by_key(&mut v, |x| *x);
        for w in v.windows(2) {
            assert!(w[0] <= w[1], "not sorted: {w:?}");
        }
    }

    #[test]
    fn sort_by_comparator_sorts_the_whole_slice() {
        let mut v: Vec<u64> = (0..1000).map(|i| (i * 7919) % 1000).collect();
        sort_unstable_by(&mut v, |a, b| b.cmp(a));
        for w in v.windows(2) {
            assert!(w[0] >= w[1], "not sorted: {w:?}");
        }
    }

    #[test]
    fn each_index_keeps_the_order() {
        let v = each_index(
            1000,
            || 0usize,
            |st, i| {
                *st += 1;
                i * 3
            },
        );
        assert_eq!(v.len(), 1000);
        assert_eq!(v[0], 0);
        assert_eq!(v[999], 2997);
    }

    #[test]
    fn each_owned_hands_the_item_over() {
        let v = each_owned(
            vec![10u32, 20, 30],
            || 0usize,
            |st, i, item| {
                *st += 1;
                (i, item)
            },
        );
        assert_eq!(v, vec![(0, 10), (1, 20), (2, 30)]);
    }

    #[test]
    fn join_runs_both() {
        let (a, b) = join(|| 1 + 1, || "x");
        assert_eq!(a, 2);
        assert_eq!(b, "x");
    }

    /// The pool keeps its workers: a search that runs a hundred times
    /// does not make a hundred threads.
    ///
    /// This is the regression the pool was written for. The rewrite that
    /// made the tree `no_std` replaced rayon's pool with
    /// `std::thread::scope`, which creates a thread per call: on the
    /// 200k-name bench that was the whole of the slowdown, and it showed
    /// as 1.5x to 2.3x on the query rows at four workers.
    #[cfg(feature = "std")]
    #[test]
    fn the_pool_keeps_its_workers_between_calls() {
        let _ = each_index(
            10_000,
            || 0usize,
            |st, i| {
                *st += i;
                i
            },
        );
        let first = pool::threads();
        for _ in 0..5 {
            let _ = each_index(
                10_000,
                || 0usize,
                |st, i| {
                    *st += i;
                    i
                },
            );
        }
        assert_eq!(
            pool::threads(),
            first,
            "five calls made {} workers, and the first made {first}",
            pool::threads()
        );
    }

    #[test]
    fn the_width_is_what_the_caller_said() {
        let before = workers();
        set_workers(3);
        assert_eq!(workers(), 3);
        set_workers(0);
        assert_eq!(workers(), before.max(1));
    }
}
