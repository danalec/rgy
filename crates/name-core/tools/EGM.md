# crates/name-core/tools

## Overview
Two Python table generators, real build-time source for the library's
generated Unicode tables. `gen_nfc_table.py` writes `nfc_table.rs` and
`nfc_vectors.rs` from the Unicode Character Database text files;
`gen_readings_table.py` writes `readings_table.rs` from Unihan readings.
Both docstrings say "Do not edit the generated file: edit this script and
re-run it", and both print provenance (UCD revision, source digests, table
sizes) that the generated headers carry.

## Interface
- `[gen_nfc_table.py:main]` — CLI entry point (`<ucd-dir> [--out DIR]`).
- `[gen_nfc_table.py:UCD_FILES]` — the four UCD inputs the script reads.
- `[gen_nfc_table.py:sha256]` — source digest helper.
- `[gen_nfc_table.py:ucd_version]`, `[gen_nfc_table.py:read_unicode_data]`,
  `[gen_nfc_table.py:read_exclusions]`, `[gen_nfc_table.py:read_qc]`,
  `[gen_nfc_table.py:read_conformance]` — UCD readers.
- `[gen_nfc_table.py:rust_table]`, `[gen_nfc_table.py:ranges]`,
  `[gen_nfc_table.py:class_ranges]`, `[gen_nfc_table.py:hexes]` — emitters.
- `[gen_readings_table.py:main]` — CLI entry point (`<unihan-dir> [--out DIR]`).
- `[gen_readings_table.py:UNIHAN]` — the single input `Unihan_Readings.txt`.
- `[gen_readings_table.py:romaji_of]`, `[gen_readings_table.py:toneless]` —
  Hepburn kana-to-romaji and tone-stripping helpers.
- `[gen_readings_table.py:parse_unihan]`, `[gen_readings_table.py:build_tables]`,
  `[gen_readings_table.py:rust_str_blob]`, `[gen_readings_table.py:emit]` —
  table construction and emission.

## Invariants
- `[gen_nfc_table.py:UCD_FILES]` — exactly four UCD files are read:
  `UnicodeData.txt`, `CompositionExclusions.txt`,
  `DerivedNormalizationProps.txt`, `NormalizationTest.txt`.
- `[gen_nfc_table.py:L_BASE]` — Hangul is composed arithmetically
  (`L_BASE/V_BASE/T_BASE`, `L/V/T/N/S_COUNT`, `S_BASE`), with no table.
- `[gen_readings_table.py:FIRST_CP]`, `[gen_readings_table.py:LAST_CP]` — the
  tables cover `0x3400..=0xFAFF`; the Plane 2 extensions are deliberately not
  carried.
- `[gen_readings_table.py:toneless]` — tone marks are stripped by NFD followed
  by dropping combining marks; the kana-to-romaji table is Hepburn, written
  here because Unihan does not carry it.

## Side Effects
- `[gen_nfc_table.py:main]` — reads the UCD text files from `<ucd-dir>`, writes
  `nfc_table.rs` and `nfc_vectors.rs` into `--out DIR`, and prints provenance.
- `[gen_nfc_table.py:sha256]` — hashes the source files for the digest the
  generated header carries.
- `[gen_readings_table.py:main]` — reads `Unihan_Readings.txt`, writes
  `readings_table.rs`, and prints provenance.
- `[gen_readings_table.py:emit]` — writes the generated Rust source file.

## Verification
- Run: `cargo test --workspace --no-fail-fast --locked`
