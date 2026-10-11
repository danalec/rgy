# rgy

Unified search stack: whole-filesystem name search (fsearch + everything-rs) and content search (rgx), on Windows, Linux, and macOS.

See `IMPLEMENTATION_PLAN.md` for the full architecture and feature matrix.

## Related projects

| Project | Path | What it provides |
|---|---|---|
| `rgx` | `../rgx` | content search engine, adapters, Tesla Valve packs |
| `fsearch` | `../fsearch` | macOS name discovery + fzf-v1 scoring |
| `everything-rs` | `../everything/everything-rs` | Windows NTFS MFT/USN indexer |
