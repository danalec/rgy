# scripts/lib

## Overview
Shared shell helpers for the scripts in `scripts/`. The three `.sh` files here
are not standalone programs: they are sourced by the caller
(`. "$here/lib/options.sh"`, `. "$here/lib/digest.sh"`,
`. "$here/lib/version.sh"`) and contribute functions to the caller's scope.
`options.sh` declares the option vocabulary every twin script shares and is
documented as the bash twin of `scripts/lib/options.ps1`; `digest.sh` resolves
the platform's SHA256 tool; `version.sh` resolves the release version out of
the tree. When `version.sh` is run as a command it prints the version of the
tree it sits in.

## Interface
- `rgy_sha256()` — prints the lowercase-hex SHA256 of a file using
  `sha256sum`, `sha256 -q` or `shasum -a 256`, whichever the platform has —
  `[digest.sh:rgy_sha256]`.
- `rgy_help_requested()` — returns true when `-h`, `--help` or `-Help` appears
  anywhere in the arguments — `[options.sh:rgy_help_requested]`.
- `rgy_option_lookup()` — prints the variable name a table gives for a
  canonical key, failing when the table does not carry it —
  `[options.sh:rgy_option_lookup]`.
- `rgy_option_nth()` — prints the variable name of the index'th positional
  slot — `[options.sh:rgy_option_nth]`.
- `rgy_read_options()` — parses `<switches> <values> <positionals> "$@"` into
  variables set in the caller's scope — `[options.sh:rgy_read_options]`.
- `rgy_release_version()` — prints `docs/release-<base>.md` or the highest
  `docs/release-<base>.<n>.md` fix of the line —
  `[version.sh:rgy_release_version]`.

## Invariants
- The `.sh`/`.ps1` twins share one option vocabulary: `--dry-run` and
  `-DryRun` are one option on either platform, and an option neither knows is
  an error — `[options.sh:--dry-run]`.
- Option names are matched with dashes and case removed, so `--dry-run`,
  `-DryRun` and `--dryrun` are one option — `[options.sh:rgy_read_options]`.
- An option in neither the switches nor the values table, or a value with
  nothing after it, prints `unknown option` and `exit 2` —
  `[options.sh:rgy_read_options]`.
- A switch is set to the numeric `1` (not `"true"`), because callers test
  `[ "$dry_run" -eq 1 ]` — `[options.sh:rgy_read_options]`.
- `rgy_sha256()` returns the same lowercase hex on Linux, macOS and FreeBSD,
  and fails with `return 1` when no SHA256 tool exists —
  `[digest.sh:rgy_sha256]`.
- `rgy_release_version()` reads the release page name rather than the manifest
  alone, and fails when the tree carries no page for the base —
  `[version.sh:rgy_release_version]`.

## Side Effects
- `rgy_sha256()` invokes an external program (`sha256sum`, `sha256` or
  `shasum`) and pipes it to `cut` — `[digest.sh:rgy_sha256]`.
- `rgy_read_options()` uses `eval` to write assignments into the caller's
  global scope — `[options.sh:rgy_read_options]`.
- `rgy_release_version()` reads `docs/release-*.md` files; when run as a
  command it reads `bins/searchctl/Cargo.toml` with `sed`/`head` —
  `[version.sh:rgy_release_version]`.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
