# Format specifications for the C port (everything-c) and other consumers.

Work items planned for Phase 6 (`IMPLEMENTATION_PLAN.md` #20):

- **ERI v3** — names blob + rows + masks + bitmaps + meta + event_id,
  mmap-loadable in ≤ 50 ms @ 5M entries. The ERI2 reader stays (see
  `crates/name-core/src/index.rs`, `ERI_MAGIC`).
- **Pack v3** — the Tesla Valve corpus store (`rgx-core` `tv/` /
  `tv_runtime/`).
- **Content segments** — trigram postings + per-file bloom layout.

Until those land, the formats are defined by their readers/writers in the
crates above; everything-c keeps reading ERI2 as it does today.
