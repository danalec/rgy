// SPDX-License-Identifier: Apache-2.0

//! The synchronisation the tree needs, without `std`.
//!
//! Two primitives, and both of them are here because there is nowhere else
//! to put them: `rgy-par` is the floor of the workspace -- it has no
//! dependency at all -- so the crates above it (the query engine, the
//! platform backends) can take a lock without dragging `std` in with it.
//!
//! The `std` build uses `std::sync::Mutex`: a thread that finds the lock
//! held parks, and the OS wakes it when the lock is free. That is the
//! right thing when the lock is held for a while.
//!
//! Without `std` there is nothing to park on, so the lock spins. It is
//! the same lock for the same jobs -- the scratch pools and the session
//! caches, whose critical sections are short -- and the API is the same
//! shape, so the call sites do not change.
//!
//! The one difference is the poison: `std`'s mutex remembers that a
//! holder panicked and says so on the next `lock()`. This one does not,
//! because there is no panic to recover from in a `no_std` build and a
//! lock that is held forever is a bug rather than a fact. `lock()`
//! therefore always succeeds, and the `Result` is there so the call
//! sites read the same either way.
//!
//! The `std` build's [`Mutex`] is `std::sync::Mutex` itself, and its
//! [`OnceCell`] is `std::sync::OnceLock`: the OS lock parks instead of
//! spinning, and the poison is the OS's. The two builds differ in that
//! one behaviour and in nothing else a caller can see.

#[cfg(not(feature = "std"))]
use core::cell::UnsafeCell;
#[cfg(not(feature = "std"))]
use core::ops::{Deref, DerefMut};
#[cfg(not(feature = "std"))]
use core::sync::atomic::{AtomicBool, AtomicU8, Ordering};

// The `std` build's own lock and cell: the OS ones, unchanged.
#[cfg(feature = "std")]
pub use std::sync::Mutex;
#[cfg(feature = "std")]
pub use std::sync::OnceLock as OnceCell;

/// A mutex that works without `std`.
#[cfg(not(feature = "std"))]
pub struct Mutex<T> {
    locked: AtomicBool,
    value: UnsafeCell<T>,
}

// The value is behind the lock, and the lock is what guards it.
#[cfg(not(feature = "std"))]
unsafe impl<T: Send> Sync for Mutex<T> {}
#[cfg(not(feature = "std"))]
unsafe impl<T: Send> Send for Mutex<T> {}

#[cfg(not(feature = "std"))]
impl<T: Default> Default for Mutex<T> {
    fn default() -> Mutex<T> {
        Mutex::new(T::default())
    }
}

#[cfg(not(feature = "std"))]
impl<T> Mutex<T> {
    /// The lock, empty.
    pub const fn new(value: T) -> Mutex<T> {
        Mutex {
            locked: AtomicBool::new(false),
            value: UnsafeCell::new(value),
        }
    }

    /// The lock, or a poisoned lock (never, here).
    pub fn lock(&self) -> Result<MutexGuard<'_, T>, Poisoned> {
        while self
            .locked
            .compare_exchange_weak(false, true, Ordering::Acquire, Ordering::Relaxed)
            .is_err()
        {
            core::hint::spin_loop();
        }
        Ok(MutexGuard { mutex: self })
    }
}

/// The guard the [`Mutex`] hands out.
#[cfg(not(feature = "std"))]
pub struct MutexGuard<'a, T> {
    mutex: &'a Mutex<T>,
}

#[cfg(not(feature = "std"))]
impl<T> Deref for MutexGuard<'_, T> {
    type Target = T;
    fn deref(&self) -> &T {
        unsafe { &*self.mutex.value.get() }
    }
}

#[cfg(not(feature = "std"))]
impl<T> DerefMut for MutexGuard<'_, T> {
    fn deref_mut(&mut self) -> &mut T {
        unsafe { &mut *self.mutex.value.get() }
    }
}

#[cfg(not(feature = "std"))]
impl<T> Drop for MutexGuard<'_, T> {
    fn drop(&mut self) {
        self.mutex.locked.store(false, Ordering::Release);
    }
}

/// The error a poisoned lock reports. Never returned here.
///
/// The `std` build reports `std::sync::PoisonError` instead: the OS lock
/// really can be poisoned, and swallowing that would lose a fact.
#[cfg(not(feature = "std"))]
#[derive(Debug)]
pub struct Poisoned;

#[cfg(not(feature = "std"))]
impl core::fmt::Display for Poisoned {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "a poisoned lock")
    }
}

#[cfg(not(feature = "std"))]
impl core::error::Error for Poisoned {}

/// A cell that is filled once, and read many times.
///
/// The `std` build uses `std::sync::OnceLock`, which parks a thread that
/// finds the cell being filled. Without `std` there is nothing to park on,
/// so the cell spins: the thread that wins the race fills it, and the
/// others wait for it.
///
/// The index's caches are filled once per index and read on every query,
/// so the spin is a first-query cost and nothing after that.
#[cfg(not(feature = "std"))]
pub struct OnceCell<T> {
    /// 0 = empty, 1 = being filled, 2 = full.
    state: AtomicU8,
    value: UnsafeCell<Option<T>>,
}

// The value is behind the cell, and the state is what guards it.
#[cfg(not(feature = "std"))]
unsafe impl<T: Sync + Send> Sync for OnceCell<T> {}
#[cfg(not(feature = "std"))]
unsafe impl<T: Sync + Send> Send for OnceCell<T> {}

#[cfg(not(feature = "std"))]
impl<T> OnceCell<T> {
    /// The cell, empty.
    pub const fn new() -> OnceCell<T> {
        OnceCell {
            state: AtomicU8::new(0),
            value: UnsafeCell::new(None),
        }
    }

    /// The value, filling the cell with `f` if it is still empty.
    pub fn get_or_init(&self, f: impl FnOnce() -> T) -> &T {
        loop {
            match self
                .state
                .compare_exchange_weak(0, 1, Ordering::Acquire, Ordering::Acquire)
            {
                Ok(_) => {
                    let v = f();
                    unsafe { *self.value.get() = Some(v) };
                    self.state.store(2, Ordering::Release);
                    return unsafe { (*self.value.get()).as_ref().unwrap_unchecked() };
                }
                Err(2) => return unsafe { (*self.value.get()).as_ref().unwrap_unchecked() },
                // Someone else is filling it. Wait for them.
                Err(_) => core::hint::spin_loop(),
            }
        }
    }

    /// The value, and the cell empty again.
    pub fn take(&mut self) -> Option<T> {
        match self
            .state
            .compare_exchange(2, 0, Ordering::AcqRel, Ordering::Acquire)
        {
            Ok(_) => unsafe { (*self.value.get()).take() },
            // Empty, or being filled: there is nothing to take, and a cell
            // being filled is left alone.
            Err(_) => None,
        }
    }

    /// The value, if the cell is full.
    ///
    /// `None` while the cell is empty *or* while another thread is filling
    /// it: a half-filled cell has no value to hand out yet.
    pub fn get(&self) -> Option<&T> {
        match self.state.load(Ordering::Acquire) {
            2 => unsafe { (*self.value.get()).as_ref() },
            _ => None,
        }
    }
}

#[cfg(not(feature = "std"))]
impl<T> Default for OnceCell<T> {
    fn default() -> OnceCell<T> {
        OnceCell::new()
    }
}
