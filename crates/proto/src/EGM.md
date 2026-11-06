# crates/proto/src

## Overview
The library sources of `rgy-proto`: hand-rolled JSON-lines protocol types
shared by `searchd`, stdio and `searchctl`. One request per line in, one
response per line out; identical over named pipe (Windows), UDS (unix) and
stdio. The crate root declares `#![no_std]` with `extern crate alloc`; the
`client` module (the only part that touches a socket) is
`#[cfg(feature = "std")]`, and `endpoint` holds the platform's daemon-location
rules.

## Interface
- `[lib.rs:no_std]` — crate root `#![no_std]` and `extern crate alloc`.
- `[lib.rs:Request]`, `[lib.rs:Response]`, `[lib.rs:CatchUp]`, `[lib.rs:Feed]`,
  `[lib.rs:Hit]`, `[lib.rs:GrepFile]`, `[lib.rs:Skip]`, `[lib.rs:LineError]`
  — the wire types.
- `[lib.rs:MAX_LIMIT]`, `[lib.rs:WIRE_LIMIT]`,
  `[lib.rs:name_answer_is_complete]`, `[lib.rs:escape_json_string]`,
  `[lib.rs:read_json]`, `[lib.rs:Json]` — limits and the JSON codec.
- `[lib.rs:pub mod endpoint]`, `[lib.rs:pub mod client]`,
  `[lib.rs:pub use client::{Client, DaemonStatus, GrepOutcome, Resolved, SearchOutcome}]`,
  `[lib.rs:pub use endpoint::Endpoint]`.
- `[endpoint.rs:SOCKET_NAME]`, `[endpoint.rs:SUN_PATH_MAX]`,
  `[endpoint.rs:Endpoint]`, `[endpoint.rs:SocketPathError]`,
  `[endpoint.rs:pipe_name_for]`, `[endpoint.rs:pipe_name]`,
  `[endpoint.rs:cache_dir_from]`, `[endpoint.rs:cache_dir]`,
  `[endpoint.rs:socket_path_in]`, `[endpoint.rs:socket_path]` — where a daemon
  listens.
- `[client.rs:Client]`, `[client.rs:ANSWER_STOPS_AT_THE_LIMIT]`,
  `[client.rs:SearchOutcome]`, `[client.rs:Resolved]`,
  `[client.rs:GrepOutcome]`, `[client.rs:DaemonStatus]`,
  `[client.rs:READ_TIMEOUT]`, `[client.rs:MAX_LINE]` — the std client.

## Invariants
- `[lib.rs:no_std]` — `no_std` + `alloc`; `extern crate std` is declared only
  behind `#[cfg(any(test, feature = "std"))]`.
- `[lib.rs:pub mod client]` — `client` is `#[cfg(feature = "std")]`; the wire
  types stay linkable into an embedded search box without it.
- `[lib.rs:MAX_LIMIT]` — `pub const MAX_LIMIT: usize = 100_000`, the most hits
  one request may ask for.
- `[lib.rs:WIRE_LIMIT]` — `pub const WIRE_LIMIT: u64 = 1 << 53`, the last
  integer the wire can carry exactly; session ids live under it.
- `[lib.rs:Request]` — `from_json` refuses a non-whole `limit`
  (`whole_number`), floors a negative one to 0, and refuses a `content`
  request with no `paths` rather than widening it into a whole-tree search.
- `[endpoint.rs:SUN_PATH_MAX]` — a unix socket path must be shorter than the
  platform's `sun_path` (108 on Linux, 104 otherwise).

## Side Effects
- `[client.rs:Client]` — opens a transport to the daemon: stdio, a unix domain
  socket, or a Windows named pipe opened with the ordinary file API.
- `[client.rs:socket_transport]`, `[client.rs:pipe_transport_within]`,
  `[client.rs:StdioTransport]` — the three transport implementations.
- `[endpoint.rs:pipe_name]` — reads `USERNAME` from the environment.
- `[endpoint.rs:cache_dir]` — reads `XDG_CACHE_HOME`, `HOME`/`USERPROFILE` and
  `LOCALAPPDATA` from the environment.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
