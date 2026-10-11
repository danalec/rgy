//! JSON-lines protocol types shared by `searchd`, stdio, and `searchctl`.
//!
//! Skeleton crate from the workspace bring-up (`IMPLEMENTATION_PLAN.md`
//! §3 / first-PR sequence item 3). The op model (`{"q", "limit"}`,
//! `{"op":"grep"|"status"|…}`) lands in Phase 5, spoken over named pipe
//! (Windows), UDS (Unix), and stdio — identical on all three.
