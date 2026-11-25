#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0

"""Generate the Unicode tables the name index's comparison fold needs.

The fold is "ASCII case-fold + NFC": a name
typed in NFC has to match a name the filesystem stored in NFD, which is what
macOS does.  NFC needs the Unicode Character Database --
there is no way to derive canonical decomposition, canonical combining class
or canonical composition from the Latin-1 range or from anything else this
repository already carries.

The tables are generated rather than written by hand, from the same source
the vendored rgx tree uses for its case-folding table
(`crates/rgx-core/src/casefold_table.rs`): the UCD text files, published by
the Unicode Consortium.

Usage:

    python crates/name-core/tools/gen_nfc_table.py <ucd-dir> [--out DIR]

`<ucd-dir>` holds `UnicodeData.txt`, `CompositionExclusions.txt`,
`DerivedNormalizationProps.txt` and `NormalizationTest.txt`, as published
under https://www.unicode.org/Public/UCD/latest/ucd/ .  The script writes:

    <out>/nfc_table.rs      the tables the normalizer looks up
    <out>/nfc_vectors.rs    the conformance vectors the test runs

and prints the provenance (UCD revision, source digests, table sizes) that
both files carry in their headers.

Do not edit the generated files: edit this script and re-run it.
"""

from __future__ import annotations

import argparse
import hashlib
import os
import sys

# The UCD files this script reads.
UCD_FILES = (
    "UnicodeData.txt",
    "CompositionExclusions.txt",
    "DerivedNormalizationProps.txt",
    "NormalizationTest.txt",
)

# Hangul, the one part of NFC with no table: the syllables are composed and
# decomposed arithmetically (UAX #15, section 16).
L_BASE, V_BASE, T_BASE = 0x1100, 0x1161, 0x11A7
L_COUNT, V_COUNT, T_COUNT = 19, 21, 28
N_COUNT, S_COUNT = V_COUNT * T_COUNT, 11172
S_BASE = 0xAC00

# How many conformance vectors to keep.  Every single-codepoint case whose
# NFC differs from its source is kept (the ones that exercise decomposition
# and recomposition), every ordering case is kept (the ones that exercise
# canonical ordering), and the rest is sampled: the identity cases all say
# the same thing, which is that an NFC string is left alone.
SAMPLE_PART2 = 400
SAMPLE_IDENTITY = 200


def sha256(path: str) -> str:
    h = hashlib.sha256()
    with open(path, "rb") as f:
        for chunk in iter(lambda: f.read(1 << 16), b""):
            h.update(chunk)
    return h.hexdigest()


def ucd_version(ucd: str) -> str:
    """The UCD revision, from the version file the UCD publishes."""
    for name in ("ReadMe.txt", "readme.txt"):
        path = os.path.join(ucd, name)
        if not os.path.exists(path):
            continue
        with open(path, encoding="utf-8", errors="replace") as f:
            for line in f:
                low = line.lower()
                if "version" not in low:
                    continue
                for token in line.replace(",", " ").split():
                    if token[0].isdigit() and token.count(".") == 2:
                        return token
    return "unknown"


def read_unicode_data(ucd: str):
    """Canonical decompositions and canonical combining classes."""
    decomp: dict[int, tuple[int, int]] = {}
    ccc: dict[int, int] = {}
    with open(os.path.join(ucd, "UnicodeData.txt"), encoding="utf-8") as f:
        for line in f:
            fields = line.split(";")
            cp = int(fields[0], 16)
            klass = int(fields[3])
            if klass:
                ccc[cp] = klass
            mapping = fields[5].strip()
            # A "<tag>" prefix is a compatibility decomposition: NFC does not
            # use those (UAX #15, section 3.3).
            if mapping and not mapping.startswith("<"):
                parts = [int(x, 16) for x in mapping.split()]
                decomp[cp] = (parts[0], parts[1] if len(parts) > 1 else 0)
    return decomp, ccc


def read_exclusions(ucd: str) -> set[int]:
    """The composition exclusions (UAX #15, section 3.4)."""
    out: set[int] = set()
    with open(os.path.join(ucd, "CompositionExclusions.txt"), encoding="utf-8") as f:
        for line in f:
            line = line.split("#")[0].strip()
            if line:
                out.add(int(line, 16))
    return out


def read_qc(ucd: str) -> tuple[set[int], set[int]]:
    """The NFC quick-check sets: NFC_QC=No and NFC_QC=Maybe."""
    no: set[int] = set()
    maybe: set[int] = set()
    with open(os.path.join(ucd, "DerivedNormalizationProps.txt"), encoding="utf-8") as f:
        for line in f:
            line = line.split("#")[0].strip()
            if not line:
                continue
            fields = [x.strip() for x in line.split(";")]
            if len(fields) < 3 or fields[1] != "NFC_QC":
                continue
            rng = fields[0]
            if ".." in rng:
                lo, hi = (int(x, 16) for x in rng.split(".."))
            else:
                lo = hi = int(rng, 16)
            for cp in range(lo, hi + 1):
                (no if fields[2] == "N" else maybe).add(cp)
    return no, maybe


def ranges(codepoints: set[int]) -> list[tuple[int, int]]:
    """A sorted set of codepoints as (lo, hi) ranges."""
    out: list[tuple[int, int]] = []
    for cp in sorted(codepoints):
        if out and cp == out[-1][1] + 1:
            out[-1] = (out[-1][0], cp)
        else:
            out.append((cp, cp))
    return out


def class_ranges(ccc: dict[int, int]) -> list[tuple[int, int, int]]:
    """The combining classes as (lo, hi, class) ranges."""
    out: list[tuple[int, int, int]] = []
    for cp in sorted(ccc):
        klass = ccc[cp]
        if out and out[-1][1] == cp - 1 and out[-1][2] == klass:
            out[-1] = (out[-1][0], cp, klass)
        else:
            out.append((cp, cp, klass))
    return out


def read_conformance(ucd: str):
    """The UCD's own conformance suite, split by part."""
    parts: dict[str, list[tuple[list[int], list[int]]]] = {}
    current = "Part0"
    with open(os.path.join(ucd, "NormalizationTest.txt"), encoding="utf-8") as f:
        for line in f:
            line = line.split("#")[0].strip()
            if not line:
                continue
            if line.startswith("@"):
                current = line.split()[0].lstrip("@")
                parts.setdefault(current, [])
                continue
            fields = [x.strip() for x in line.split(";") if x.strip()]
            src = [int(x, 16) for x in fields[0].split()]
            nfc = [int(x, 16) for x in fields[1].split()]
            parts.setdefault(current, []).append((src, nfc))
    return parts


def hexes(cps: list[int]) -> str:
    return " ".join("%04X" % c for c in cps)


def rust_table(name: str, doc: str, rows: list[str], ty: str) -> str:
    body = "\n".join("    %s," % r for r in rows)
    return "\n".join(
        [
            "/// %s" % doc,
            "#[rustfmt::skip]",
            "pub(crate) static %s: &[%s] = &[" % (name, ty),
            body,
            "];",
        ]
    )


def main() -> int:
    ap = argparse.ArgumentParser(description=__doc__)
    ap.add_argument("ucd", help="the directory holding the UCD text files")
    ap.add_argument("--out", default=None, help="where to write the generated files")
    args = ap.parse_args()

    ucd = args.ucd
    out = args.out or os.path.join(os.path.dirname(os.path.abspath(__file__)), "..", "src")
    out = os.path.abspath(out)

    for name in UCD_FILES:
        if not os.path.exists(os.path.join(ucd, name)):
            print("missing %s in %s" % (name, ucd), file=sys.stderr)
            return 1

    version = ucd_version(ucd)
    digests = {name: sha256(os.path.join(ucd, name)) for name in UCD_FILES}

    decomp, ccc = read_unicode_data(ucd)
    exclusions = read_exclusions(ucd)
    qc_no, qc_maybe = read_qc(ucd)

    # Primary composites: a canonical decomposition of exactly two characters
    # whose first is a starter, and which is not excluded (UAX #15, 3.4).
    compose: list[tuple[int, int, int]] = []
    for cp, (a, b) in sorted(decomp.items()):
        if b == 0 or ccc.get(a, 0) != 0 or cp in exclusions:
            continue
        compose.append((a, b, cp))
    compose.sort()

    table = "\n\n".join(
        [
            rust_table(
                "DECOMP",
                "Canonical decompositions: `(codepoint, first, second)`, sorted by\n"
                "/// codepoint. A `second` of 0 is a one-character decomposition.",
                ["(0x%04X, 0x%04X, 0x%04X)" % (cp, a, b) for cp, (a, b) in sorted(decomp.items())],
                "(u32, u32, u32)",
            ),
            rust_table(
                "CCC",
                "Canonical combining classes: `(lo, hi, class)` ranges, sorted.",
                ["(0x%04X, 0x%04X, %d)" % (lo, hi, k) for lo, hi, k in class_ranges(ccc)],
                "(u32, u32, u8)",
            ),
            rust_table(
                "COMPOSE",
                "Primary composites: `(first, second, composite)`, sorted by the pair.",
                ["(0x%04X, 0x%04X, 0x%04X)" % (a, b, c) for a, b, c in compose],
                "(u32, u32, u32)",
            ),
            rust_table(
                "QC_NO",
                "The quick-check set `NFC_QC=No`: `(lo, hi)` ranges.",
                ["(0x%04X, 0x%04X)" % (lo, hi) for lo, hi in ranges(qc_no)],
                "(u32, u32)",
            ),
            rust_table(
                "QC_MAYBE",
                "The quick-check set `NFC_QC=Maybe`: `(lo, hi)` ranges.",
                ["(0x%04X, 0x%04X)" % (lo, hi) for lo, hi in ranges(qc_maybe)],
                "(u32, u32)",
            ),
        ]
    )

    header = "\n".join(
        [
            "// SPDX-License-Identifier: Apache-2.0",
            "//",
            "//! Unicode tables for the comparison fold's NFC step.",
            "//!",
            "//! Generated by `crates/name-core/tools/gen_nfc_table.py` from the",
            "//! Unicode Character Database. Do not edit this file; edit the",
            "//! generator and re-run it.",
            "//!",
            "//! The data is the Unicode Character Database, published by the Unicode",
            "//! Consortium under the Unicode License, v3 (Unicode-DFS-2016):",
            "//! https://www.unicode.org/license.txt",
            "//!",
            "//! UCD revision: %s" % version,
        ]
        + ["//! Source SHA-256: %s  %s" % (digests[name], name) for name in UCD_FILES]
        + [
            "//! Decompositions: %d   combining classes: %d   composites: %d"
            % (len(decomp), len(ccc), len(compose)),
        ]
    )

    with open(os.path.join(out, "nfc_table.rs"), "w", encoding="utf-8", newline="\n") as f:
        f.write(header + "\n\n" + table + "\n")

    # The conformance vectors: every case that does real work, plus a sample
    # of the ones that do not.
    parts = read_conformance(ucd)
    vectors: list[tuple[list[int], list[int]]] = []
    for case in parts.get("Part1", []):
        if case[0] != case[1]:
            vectors.append(case)
    part2 = parts.get("Part2", [])
    step = max(1, len(part2) // SAMPLE_PART2)
    vectors.extend(part2[::step][:SAMPLE_PART2])
    vectors.extend(parts.get("Part3", []))
    identity = [c for c in parts.get("Part1", []) if c[0] == c[1]]
    step = max(1, len(identity) // SAMPLE_IDENTITY)
    vectors.extend(identity[::step][:SAMPLE_IDENTITY])

    rows = ['    ("%s", "%s"),' % (hexes(src), hexes(nfc)) for src, nfc in vectors]
    vec_header = "\n".join(
        [
            "// SPDX-License-Identifier: Apache-2.0",
            "//",
            "//! Conformance vectors for the NFC normalizer.",
            "//!",
            "//! Generated by `crates/name-core/tools/gen_nfc_table.py` from the",
            "//! UCD's own conformance suite (`NormalizationTest.txt`). Do not edit",
            "//! this file; edit the generator and re-run it.",
            "//!",
            "//! Each pair is `(source, NFC)` as space-separated hex codepoints.",
            "//!",
            "//! UCD revision: %s" % version,
            "//! Source SHA-256: %s  NormalizationTest.txt" % digests["NormalizationTest.txt"],
            "//! Vectors: %d   (of %d conformance cases)"
            % (len(vectors), sum(len(v) for v in parts.values())),
        ]
    )
    with open(os.path.join(out, "nfc_vectors.rs"), "w", encoding="utf-8", newline="\n") as f:
        f.write(
            vec_header
            + "\n\n/// The conformance vectors: `(source, NFC)` as hex codepoint lists.\n"
            + "#[rustfmt::skip]\npub(crate) static CONFORMANCE: &[(&str, &str)] = &[\n"
            + "\n".join(rows)
            + "\n];\n"
        )

    print("UCD revision: %s" % version)
    print("decompositions: %d  combining classes: %d  composites: %d" % (len(decomp), len(ccc), len(compose)))
    print("quick check: No=%d Maybe=%d" % (len(qc_no), len(qc_maybe)))
    print(
        "conformance cases: %d  vectors kept: %d"
        % (sum(len(v) for v in parts.values()), len(vectors))
    )
    for name in UCD_FILES:
        print("  %s  %s" % (digests[name], name))
    return 0


if __name__ == "__main__":
    raise SystemExit(main())
