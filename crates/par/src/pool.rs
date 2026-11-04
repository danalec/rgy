// SPDX-License-Identifier: Apache-2.0

//! The pool: the workers that live between the calls.
//!
//! The tree's parallelism is written here, not borrowed, and this
//! is where the pool lives: the threads that carry the tree's parallel
//! work are created once and kept, so a search that runs a hundred times
//! does not create a hundred threads.
//!
//! **Why this matters.** The pool this crate replaces was persistent, and
//! the rewrite replaced it with `std::thread::scope` -- which creates a
//! thread per call. On the 200k-name bench that was the whole of the
//! regression: at one worker the new tree matched the old one to within
//! 3%, and at four it was 1.5x to 2.3x slower, because every parallel
//! call paid for three thread creations.
//!
//! **The shape.** A job is a closure the caller runs too: the calling
//! thread is one of the workers, and the pool's workers run the same
//! closure, each drawing its share from the counter the closure owns. The
//! caller waits for every worker to be done before it returns.
//!
//! **The borrow.** The closure borrows -- the index, the scan's scratch,
//! the caller's buffer -- and a job cannot borrow. The job carries the
//! closure as a pointer, and the caller's wait is what makes that sound:
//! the borrow outlives the run, because the caller does not let go of it
//! until the last worker is finished.
//!
//! **The nesting.** A job inside a job runs on the thread that asked, if
//! the pool has nobody free: the workers are busy with the outer job, and
//! taking one of them for the inner one would leave the outer job waiting
//! for a worker that is waiting for it. The inner job runs alone, and the
//! answer is the same.

use alloc::boxed::Box;
use alloc::string::String;
use alloc::vec::Vec;
use core::any::Any;
use core::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard, OnceLock};

/// How many times a worker looks for work before it parks.
///
/// A search is a burst of parallel calls a few microseconds apart, and a
/// worker that parks between them pays a wake-up for each. The spin is
/// what makes this pool as cheap as the one it replaces: the worker is
/// already running when the next call comes.
const SPIN: u32 = 512;

/// A job: what the workers run, and who may still take it.
struct Job {
    /// The job's id: the caller and the workers find it by this.
    id: u64,
    /// The function that runs it.
    run: unsafe fn(*mut ()),
    /// What it runs on: the caller's closure, as a pointer.
    data: *mut (),
    /// How many workers may still take this job.
    permits: usize,
    /// The workers running it right now.
    running: usize,
    /// Whether the caller has closed it: no worker may take it any more.
    closed: bool,
    /// The panic one of the workers took, for the caller to raise.
    panic: Option<Box<dyn Any + Send>>,
}

// Safety: the pointer is dereferenced only while the job runs, and the
// caller that owns it does not return until every worker is done.
unsafe impl Send for Job {}

/// The pool's state, under the lock.
struct State {
    /// The open jobs.
    jobs: Vec<Job>,
    /// The next job id.
    next_id: u64,
    /// The worker threads alive.
    threads: usize,
}

impl State {
    /// The first job a worker may take.
    fn takeable(&self) -> Option<usize> {
        self.jobs.iter().position(|j| !j.closed && j.permits > 0)
    }

    /// The job with this id.
    fn job_mut(&mut self, id: u64) -> Option<&mut Job> {
        self.jobs.iter_mut().find(|j| j.id == id)
    }
}

/// The pool: the workers, and the jobs they are given.
pub(crate) struct Pool {
    /// The jobs, the thread count, under the lock.
    state: Mutex<State>,
    /// The workers wait here.
    wake: Condvar,
    /// Whether there is a job the workers have not taken yet.
    ///
    /// The flag is a hint: it lets a spinning worker look for work
    /// without taking the lock. It is set when a job is installed and
    /// cleared when a worker is about to park, so a worker that sees it
    /// set knows there may be something to take.
    pending: AtomicBool,
}

/// The pool, once.
static POOL: OnceLock<Pool> = OnceLock::new();

/// The pool.
fn pool() -> &'static Pool {
    POOL.get_or_init(|| Pool {
        state: Mutex::new(State {
            jobs: Vec::new(),
            next_id: 0,
            threads: 0,
        }),
        wake: Condvar::new(),
        pending: AtomicBool::new(false),
    })
}

/// The lock, without the poison.
///
/// A worker that panicked while holding it has already done its damage;
/// the pool is still usable, and refusing to work because of it would
/// turn one failure into a dead process.
fn lock(state: &Mutex<State>) -> MutexGuard<'_, State> {
    state.lock().unwrap_or_else(|e| e.into_inner())
}

/// Run a job on the pool: the calling thread runs it too, and up to
/// `helpers` of the pool's workers run it as well.
///
/// The job's workers draw their share from the counter the job owns, so
/// the job is written to be run by more than one thread at a time.
///
/// # Safety
///
/// `data` must point to a value that stays valid, and is not changed,
/// until this call returns: the pool's workers dereference it while the
/// job runs, and this call does not return until the last of them is
/// done.
pub(crate) unsafe fn run(helpers: usize, run: unsafe fn(*mut ()), data: *mut ()) {
    let pool = pool();
    let id = {
        let mut st = lock(&pool.state);
        let id = st.next_id;
        st.next_id += 1;
        st.jobs.push(Job {
            id,
            run,
            data,
            permits: helpers,
            running: 0,
            closed: false,
            panic: None,
        });
        // The pool grows to the width the job wants, and keeps the threads
        // for the next one.
        while st.threads < helpers {
            match spawn(pool) {
                Ok(()) => st.threads += 1,
                // No more threads: the job runs on fewer workers, which is
                // slower and still right.
                Err(_) => break,
            }
        }
        pool.pending.store(true, Ordering::Release);
        id
    };
    pool.wake.notify_all();

    // The calling thread is one of the workers.
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe { run(data) }));

    // Close the job and wait for the workers that took it.
    let mut st = lock(&pool.state);
    if let Some(job) = st.job_mut(id) {
        job.closed = true;
        job.permits = 0;
    }
    while st.job_mut(id).map(|j| j.running).unwrap_or(0) > 0 {
        st = pool.wake.wait(st).unwrap_or_else(|e| e.into_inner());
    }
    let panic = st.job_mut(id).and_then(|j| j.panic.take());
    st.jobs.retain(|j| j.id != id);
    drop(st);

    // A worker's panic is the caller's, the way a scoped thread's is.
    if let Some(p) = panic {
        std::panic::resume_unwind(p);
    }
    if let Err(p) = result {
        std::panic::resume_unwind(p);
    }
}

/// A worker thread: take a job, run it, take the next.
fn worker(pool: &'static Pool) {
    let mut spins = 0u32;
    loop {
        // Is there work? The flag says so without the lock, and the lock
        // is what the installer needs: a worker that spins must not
        // stand in its way.
        if pool.pending.load(Ordering::Acquire) {
            let mut st = lock(&pool.state);
            if let Some(i) = st.takeable() {
                let (run, data, id) = {
                    let job = &mut st.jobs[i];
                    job.permits -= 1;
                    job.running += 1;
                    (job.run, job.data, job.id)
                };
                drop(st);
                let result =
                    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| unsafe { run(data) }));
                let mut st = lock(&pool.state);
                if let Some(job) = st.job_mut(id) {
                    job.running -= 1;
                    if let Err(p) = result
                        && job.panic.is_none()
                    {
                        job.panic = Some(p);
                    }
                }
                pool.wake.notify_all();
                spins = 0;
                continue;
            }
            // Nothing to take after all: the flag was stale.
            pool.pending.store(false, Ordering::Release);
            drop(st);
        }

        // Nothing to take. Look again a few times before parking: the
        // next call of a burst is usually a few microseconds away.
        if spins < SPIN {
            spins += 1;
            core::hint::spin_loop();
            continue;
        }

        // Park until a job arrives.
        let mut st = lock(&pool.state);
        if st.takeable().is_some() {
            spins = 0;
            continue;
        }
        pool.pending.store(false, Ordering::Release);
        st = pool.wake.wait(st).unwrap_or_else(|e| e.into_inner());
        spins = 0;
    }
}

/// How many worker threads the pool has.
///
/// The pool's threads are kept, so this does not grow with the number of
/// calls: a search that runs a hundred times leaves it where one call
/// did.
#[cfg(test)]
pub(crate) fn threads() -> usize {
    lock(&pool().state).threads
}

/// A worker thread, on the pool.
fn spawn(pool: &'static Pool) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(String::from("rgy-worker"))
        .spawn(move || worker(pool))?;
    Ok(())
}
