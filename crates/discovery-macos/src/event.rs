// SPDX-License-Identifier: Apache-2.0

//! What an FSEvents stream delivers: the event, and the flags that come
//! with it.
//!
//! Plain data on every platform. The decisions that read these flags — which
//! folder an event means, whether the feed lost history — have to be
//! testable where FSEvents does not run, so they live beside the type
//! rather than behind the FFI.

use alloc::vec::Vec;

/// The framework dropped events: the batch is not the whole truth.
pub const KERNEL_DROPPED: u32 = 0x0000_0004;
/// The client fell behind: the batch is not the whole truth.
pub const USER_DROPPED: u32 = 0x0000_0002;
/// The folder's subtree changed in a way the framework coalesced: the
/// events for that subtree are not in the batch, and the subtree has to be
/// scanned.
pub const MUST_SCAN_SUBDIRS: u32 = 0x0000_0001;
/// The history the stream was opened for is complete: the stream has caught
/// up with the point it was asked for.
pub const HISTORY_DONE: u32 = 0x0000_0010;
/// The item the event names is a directory. Only set on a stream created
/// with `kFSEventStreamCreateFlagFileEvents`.
pub const ITEM_IS_DIR: u32 = 0x0002_0000;

/// One event from an FSEvents stream.
#[derive(Clone, Debug)]
pub struct Event {
    /// The path the event is about: absolute, as the framework spells it.
    pub path: Vec<u8>,
    /// The event's flags ([`MUST_SCAN_SUBDIRS`] and friends).
    pub flags: u32,
    /// The event's id: the stream's place in the change history.
    pub id: u64,
    /// Which `watch` delivered it (its `stream` number).
    pub stream: u32,
}
