// SPDX-License-Identifier: Apache-2.0

//! The synchronisation the query engine needs.
//!
//! The primitives live in `rgy-par`: that crate is the floor of the tree
//! -- it has no dependency at all -- and the platform backends need the
//! same lock and the same cell. This module is only the name the query
//! engine says them by.
//!
//! Under `std` they are `std::sync::Mutex` and `std::sync::OnceLock`; a
//! `no_std` build gets the spinning ones. The call sites read the same
//! either way.

pub(crate) use rgy_par::sync::{Mutex, OnceCell};
