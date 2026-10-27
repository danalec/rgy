// SPDX-License-Identifier: Apache-2.0

//! A channel, with and without `std`.
//!
//! The FSEvents feed is a callback feed and the daemon speaks a pull: the
//! kernel pushes batches at the crate and the daemon pulls them one at a
//! time. Something has to hold the batches in between, and that something
//! is a channel.
//!
//! Under `std` it is `std::sync::mpsc`: the reader that finds the queue
//! empty parks, and the OS wakes it when a batch arrives. Without `std`
//! there is nothing to park on, so the reader spins. The queue is short
//! (a batch or two, between the kernel's callback and the daemon's next
//! poll) and the reader is the daemon's own thread, so the spin is a
//! fraction of the daemon's life -- and the `no_std` build is the one
//! that never runs against a real kernel anyway.
//!
//! The API is the same in both builds: [`channel`], [`Sender::send`],
//! [`Receiver::recv`] and [`Receiver::try_recv`]. The error types are
//! `std`'s under `std` and this crate's own without it, and both say the
//! same things: the queue is empty, or the other end is gone.

#[cfg(not(feature = "std"))]
use crate::sync::Mutex;
#[cfg(not(feature = "std"))]
use alloc::collections::VecDeque;
#[cfg(not(feature = "std"))]
use alloc::sync::Arc;
#[cfg(not(feature = "std"))]
use core::sync::atomic::{AtomicBool, Ordering};

// The `std` build's channel: the OS one, unchanged.
#[cfg(feature = "std")]
pub use std::sync::mpsc::{Receiver, RecvError, SendError, Sender, TryRecvError, channel};

/// The queue both ends share.
#[cfg(not(feature = "std"))]
struct Shared<T> {
    queue: Mutex<VecDeque<T>>,
    /// Set when the sender is gone: the reader has to be told, rather than
    /// spinning on a queue that will never fill again.
    closed: AtomicBool,
}

/// The sending end.
///
/// Cloning it is how two producers share a queue; the queue is closed when
/// every sender is gone.
#[cfg(not(feature = "std"))]
pub struct Sender<T> {
    inner: Arc<Shared<T>>,
}

/// The receiving end.
#[cfg(not(feature = "std"))]
pub struct Receiver<T> {
    inner: Arc<Shared<T>>,
}

/// The ends are `Debug` whatever the value is, like `std`'s.
#[cfg(not(feature = "std"))]
impl<T> core::fmt::Debug for Sender<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Sender { .. }")
    }
}

/// The ends are `Debug` whatever the value is, like `std`'s.
#[cfg(not(feature = "std"))]
impl<T> core::fmt::Debug for Receiver<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        f.write_str("Receiver { .. }")
    }
}

/// A channel: the sending end and the receiving end of one queue.
#[cfg(not(feature = "std"))]
pub fn channel<T>() -> (Sender<T>, Receiver<T>) {
    let inner = Arc::new(Shared {
        queue: Mutex::new(VecDeque::new()),
        closed: AtomicBool::new(false),
    });
    (
        Sender {
            inner: inner.clone(),
        },
        Receiver { inner },
    )
}

#[cfg(not(feature = "std"))]
impl<T> Sender<T> {
    /// Put a value on the queue.
    ///
    /// `Err` carries the value back when the receiver is gone: a value
    /// nobody will read is the caller's to decide about, not to lose.
    pub fn send(&self, value: T) -> Result<(), SendError<T>> {
        if self.inner.closed.load(Ordering::Acquire) {
            return Err(SendError(value));
        }
        self.inner.queue.lock().unwrap().push_back(value);
        Ok(())
    }
}

#[cfg(not(feature = "std"))]
impl<T> Receiver<T> {
    /// The next value, waiting for one.
    ///
    /// The wait is a spin: without `std` there is nothing to park on.
    /// `Err` when every sender is gone and the queue is empty.
    pub fn recv(&self) -> Result<T, RecvError> {
        loop {
            match self.try_recv() {
                Ok(v) => return Ok(v),
                Err(TryRecvError::Empty) => {
                    if self.inner.closed.load(Ordering::Acquire) {
                        return Err(RecvError);
                    }
                    core::hint::spin_loop();
                }
                Err(TryRecvError::Disconnected) => return Err(RecvError),
            }
        }
    }

    /// The next value, if there is one right now.
    pub fn try_recv(&self) -> Result<T, TryRecvError> {
        let mut q = self.inner.queue.lock().unwrap();
        if let Some(v) = q.pop_front() {
            return Ok(v);
        }
        // The sender marks the queue closed when it is dropped; the reader
        // also sees the count fall to one.
        if self.inner.closed.load(Ordering::Acquire) || Arc::strong_count(&self.inner) == 1 {
            Err(TryRecvError::Disconnected)
        } else {
            Err(TryRecvError::Empty)
        }
    }
}

/// The sender is gone, and the value never arrived.
#[cfg(not(feature = "std"))]
#[derive(Debug)]
pub struct SendError<T>(pub T);

#[cfg(not(feature = "std"))]
impl<T> core::fmt::Display for SendError<T> {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "the receiving end is gone")
    }
}

/// Every sender is gone, and the queue is empty.
#[cfg(not(feature = "std"))]
#[derive(Debug)]
pub struct RecvError;

#[cfg(not(feature = "std"))]
impl core::fmt::Display for RecvError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        write!(f, "the sending end is gone")
    }
}

/// The queue is empty -- or it is closed, and will stay empty.
#[cfg(not(feature = "std"))]
#[derive(Debug, PartialEq, Eq)]
pub enum TryRecvError {
    /// Nothing right now; try again later.
    Empty,
    /// Every sender is gone.
    Disconnected,
}

#[cfg(not(feature = "std"))]
impl core::fmt::Display for TryRecvError {
    fn fmt(&self, f: &mut core::fmt::Formatter<'_>) -> core::fmt::Result {
        match self {
            TryRecvError::Empty => write!(f, "the queue is empty"),
            TryRecvError::Disconnected => write!(f, "the sending end is gone"),
        }
    }
}

/// The sender tells the reader when it goes.
///
/// The reader also watches the count of the ends it shares, so a sender
/// that is dropped without saying anything still closes the queue -- but
/// saying it is what lets the reader stop spinning at once.
#[cfg(not(feature = "std"))]
impl<T> Drop for Sender<T> {
    fn drop(&mut self) {
        if Arc::strong_count(&self.inner) == 1 {
            self.inner.closed.store(true, Ordering::Release);
        }
    }
}
