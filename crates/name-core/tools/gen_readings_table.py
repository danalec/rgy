#!/usr/bin/env python3
# SPDX-License-Identifier: Apache-2.0

"""Generate the reading tables the transliteration matchers need.

A query typed in one script has to match a name written in another:
`henshin` has to find 日本語, `にほんご` has to find 日本語, and `zhongwen`
has to find 中文.  That means every name needs a *shadow*: a transliteration
of its characters into a script the query can be in, with a map back to the
source characters so the match can be reported against the name the user
sees.

The shadows come from the Unicode Character Database's Unihan readings:

  kJapanese    the readings of a Han character, in kana (the on-readings
               in katakana, the kun-readings in hiragana)
  kMandarin    the readings of a Han character, in pinyin

Both readings are written with tone marks ("zhōng", "rì"); the matcher
compares against an unaccented query, so the marks are stripped with the
UCD's own decomposition rules (NFD, then drop the combining marks).

Unihan does not carry a kana-to-romaji table -- it gives readings in kana
and readings in romaji, but not the mapping between them -- so that table
is written here.  It is Hepburn, the way the romaji IME of every platform
types: `shi`, `chi`, `tsu`, `fu`, `ji`, with the small kana and the long
vowel mark resolved the way a person types them.

Usage:

    python crates/name-core/tools/gen_readings_table.py <unihan-dir> [--out DIR]

`<unihan-dir>` holds Unihan_Readings.txt, as published under
https://www.unicode.org/Public/UCD/latest/ucd/ .  The script writes:

    <out>/readings_table.rs    the tables the shadow looks up

and prints the provenance (UCD revision, source digest, table sizes) that
the file carries in its header.

Do not edit the generated file: edit this script and re-run it.
"""

from __future__ import annotations

import argparse
import hashlib
import sys
import unicodedata
from pathlib import Path

# The file this script reads.
UNIHAN = "Unihan_Readings.txt"

# The codepoint range the tables cover: the CJK Extension A block, the main
# block and the compatibility ideographs.  A dense array over it is four
# bytes a character; the Plane 2 extensions are sparse and rare in a
# filename, so they are not carried here (the generated file says so).
FIRST_CP = 0x3400
LAST_CP = 0xFAFF

# --------------------------------------------------------------------------
# The kana-to-romaji table.  Hepburn, IME style.
#
# The small kana and the long vowel mark are not in this table: they are
# resolved by the rules in `romaji_of`, because they depend on what
# precedes them.
# --------------------------------------------------------------------------

_HIRAGANA = {
    "あ": "a", "い": "i", "う": "u", "え": "e", "お": "o",
    "ぁ": "a", "ぃ": "i", "ぅ": "u", "ぇ": "e", "ぉ": "o",
    "か": "ka", "き": "ki", "く": "ku", "け": "ke", "こ": "ko",
    "が": "ga", "ぎ": "gi", "ぐ": "gu", "げ": "ge", "ご": "go",
    "さ": "sa", "し": "shi", "す": "su", "せ": "se", "そ": "so",
    "ざ": "za", "じ": "ji", "ず": "zu", "ぜ": "ze", "ぞ": "zo",
    "た": "ta", "ち": "chi", "つ": "tsu", "て": "te", "と": "to",
    "だ": "da", "ぢ": "ji", "づ": "zu", "で": "de", "ど": "do",
    "な": "na", "に": "ni", "ぬ": "nu", "ね": "ne", "の": "no",
    "は": "ha", "ひ": "hi", "ふ": "fu", "へ": "he", "ほ": "ho",
    "ば": "ba", "び": "bi", "ぶ": "bu", "べ": "be", "ぼ": "bo",
    "ぱ": "pa", "ぴ": "pi", "ぷ": "pu", "ぺ": "pe", "ぽ": "po",
    "ま": "ma", "み": "mi", "む": "mu", "め": "me", "も": "mo",
    "や": "ya", "ゆ": "yu", "よ": "yo",
    "ら": "ra", "り": "ri", "る": "ru", "れ": "re", "ろ": "ro",
    "わ": "wa", "ゐ": "wi", "ゑ": "we", "を": "wo",
    "ん": "n",
    "ゔ": "vu",
}

# The katakana is the hiragana shifted by 0x60, with the exceptions the
# shift does not cover (the small ones and the archaic ones).
_KATAKANA = {}
for _h, _r in _HIRAGANA.items():
    _k = chr(ord(_h) + 0x60)
    if 0x30A1 <= ord(_k) <= 0x30FF:
        _KATAKANA[_k] = _r
_KATAKANA["ヷ"] = "vi"
_KATAKANA["ヸ"] = "ve"
_KATAKANA["ヹ"] = "vo"
_KATAKANA["ヺ"] = "va"

KANA = {**_HIRAGANA, **_KATAKANA}

# The small kana that combine with the kana before them.
SMALL_Y = "ゃャ"   # ya
SMALL_U = "ゅュ"   # yu
SMALL_O = "ょョ"   # yo
# The kana whose romaji ends in a consonant before a y: `sha`, not `shya`.
_IRREGULAR = {"し": "sh", "シ": "sh", "ち": "ch", "チ": "ch", "じ": "j", "ジ": "j"}
# The small tsu, which doubles the consonant that follows it.
SOKUON = "っッ"
# The long vowel mark, which repeats the vowel before it.
LONG = "ー―"

_VOWELS = "aeiou"


def _consonant_prefix(romaji: str) -> str:
    """The part of a kana's romaji that a small ya/yu/yo joins."""
    if romaji.endswith("i"):
        # `shi` -> `sh`, `ki` -> `k`, `ji` -> `j`.
        for k, v in _IRREGULAR.items():
            if KANA.get(k) == romaji:
                return v
        return romaji[:-1]
    return romaji


def romaji_of(kana: str) -> str:
    """One kana in romaji, on its own.

    The small kana and the long vowel mark are resolved by the caller,
    because they depend on what precedes them.
    """
    return KANA.get(kana, "")


def _is_vowel(ch: str) -> bool:
    return ch in _VOWELS


def toneless(s: str) -> str:
    """Drop the tone mark: 'zhōng' -> 'zhong', 'rì' -> 'ri'."""
    return "".join(c for c in unicodedata.normalize("NFD", s)
                   if not unicodedata.combining(c))


def parse_unihan(path: Path):
    """The readings this script needs, out of Unihan_Readings.txt."""
    kana_readings: dict[int, list[str]] = {}
    pinyin: dict[int, list[str]] = {}
    digest = hashlib.sha256()
    revision = ""
    with path.open("rb") as fh:
        for chunk in iter(lambda: fh.read(1 << 20), b""):
            digest.update(chunk)
    for line in path.read_text(encoding="utf-8").splitlines():
        if line.startswith("#"):
            if line.startswith("# Date:"):
                # "# Date: 2026-07-31 00:00:00 GMT [KL]" -- the date has
                # colons in it, so split once and take the rest.
                revision = line.split(":", 1)[-1].strip()
            continue
        if not line.strip():
            continue
        parts = line.split("\t")
        if len(parts) < 3:
            continue
        cp = int(parts[0][2:], 16)
        field, value = parts[1], parts[2]
        if not (FIRST_CP <= cp <= LAST_CP):
            continue
        if field == "kJapanese":
            kana_readings[cp] = value.split()
        elif field == "kMandarin":
            pinyin[cp] = [toneless(v) for v in value.split()]
    return kana_readings, pinyin, digest.hexdigest(), revision


def build_tables(kana_readings, pinyin):
    """The dense tables, and the string blobs they point into."""
    kana_ids: dict[str, int] = {}
    kana_blob: list[str] = []
    pinyin_ids: dict[str, int] = {}
    pinyin_blob: list[str] = []

    def intern(blob, ids, s):
        i = ids.get(s)
        if i is None:
            i = len(blob)
            ids[s] = i
            blob.append(s)
        return i

    n = LAST_CP - FIRST_CP + 1
    kana_tab = [0] * n
    pinyin_tab = [0] * n
    for cp, readings in kana_readings.items():
        # The first reading Unihan lists is the on-reading (in katakana), or
        # the kun-reading when there is no on-reading. One reading per
        # character is what a name is read as: a compound word is read
        # character by character, and the alternatives are a different
        # reading of the same character, not the reading of the name.
        kana_tab[cp - FIRST_CP] = intern(kana_blob, kana_ids, readings[0]) + 1
    for cp, readings in pinyin.items():
        pinyin_tab[cp - FIRST_CP] = intern(pinyin_blob, pinyin_ids, readings[0]) + 1
    return kana_tab, kana_blob, pinyin_tab, pinyin_blob


def rust_str_blob(blob) -> str:
    """The blob as a Rust array, one string per line.

    `#[rustfmt::skip]` keeps it as it is: rustfmt would otherwise pack
    the short strings several to a line, and the generated file has to
    survive the check the CI runs.
    """
    out = []
    for s in blob:
        esc = "".join("\\u{%x}" % ord(c) if ord(c) > 0x7E or ord(c) < 0x20 else c
                       for c in s)
        out.append('    "%s",' % esc.replace('"', '\\"'))
    return "&[\n" + "\n".join(out) + "\n]"


def emit(out: Path, kana_tab, kana_blob, pinyin_tab, pinyin_blob, digest, revision):
    lines = []
    w = lines.append
    w("// SPDX-License-Identifier: Apache-2.0")
    w("//")
    w("//! Readings for the transliteration matchers: what a Han character is")
    w("//! called in kana, and what it is called in pinyin.")
    w("//!")
    w("//! Generated by `crates/name-core/tools/gen_readings_table.py` from the")
    w("//! Unicode Character Database. Do not edit this file; edit the")
    w("//! generator and re-run it.")
    w("//!")
    w("//! The data is the Unicode Character Database, published by the")
    w("//! Unicode Consortium under the Unicode License, v3 (Unicode-DFS-2016):")
    w("//! https://www.unicode.org/license.txt")
    w("//!")
    w("//! UCD revision: %s" % (revision or "unknown"))
    w("//! Source SHA-256: %s  %s" % (digest, UNIHAN))
    w("//! Coverage: U+%04X..U+%04X (the CJK Extension A block, the main block" % (FIRST_CP, LAST_CP))
    w("//! and the compatibility ideographs). The Plane 2 extensions are not")
    w("//! carried: they are sparse and rare in a filename, and a dense table")
    w("//! over the BMP is four bytes a character.")
    w("")
    w("/// The first codepoint the tables cover.")
    w("pub const FIRST_CP: u32 = 0x%04X;" % FIRST_CP)
    w("/// One past the last codepoint the tables cover.")
    w("pub const LAST_CP: u32 = 0x%04X;" % (LAST_CP + 1))
    w("")
    w("/// The kana, in hiragana and katakana, and what each is in romaji.")
    w("///")
    w("/// The small kana and the long vowel mark are not here: they are")
    w("/// resolved against what precedes them, in `readings.rs`.")
    w("#[rustfmt::skip]")
    w("pub const KANA: &[(&str, &str, &str)] = &[")
    for h, r in sorted(_HIRAGANA.items()):
        k = chr(ord(h) + 0x60)
        if 0x30A1 <= ord(k) <= 0x30FF:
            w('    ("%s", "%s", "%s"),' % (h, k, r))
    w("];")
    w("")
    w("/// The kana readings, as a blob. The index into it is one less than the")
    w("/// id a table holds.")
    w("#[rustfmt::skip]")
    w("pub const KANA_READINGS: &[&str] = %s;" % rust_str_blob(kana_blob))
    w("")
    w("/// The pinyin readings, as a blob.")
    w("#[rustfmt::skip]")
    w("pub const PINYIN_READINGS: &[&str] = %s;" % rust_str_blob(pinyin_blob))
    w("")
    w("/// Per codepoint, the reading's id plus one. Zero means no reading.")
    w("#[rustfmt::skip]")
    w("pub const KANA_TABLE: &[u32] = &[")
    for i in range(0, len(kana_tab), 16):
        w("    " + " ".join("%d," % v for v in kana_tab[i:i + 16]))
    w("];")
    w("")
    w("/// Per codepoint, the pinyin reading's id plus one. Zero means no reading.")
    w("#[rustfmt::skip]")
    w("pub const PINYIN_TABLE: &[u32] = &[")
    for i in range(0, len(pinyin_tab), 16):
        w("    " + " ".join("%d," % v for v in pinyin_tab[i:i + 16]))
    w("];")
    out.write_text("\n".join(lines) + "\n", encoding="utf-8")
    return len(lines)


def main() -> int:
    ap = argparse.ArgumentParser()
    ap.add_argument("unihan_dir", type=Path)
    ap.add_argument("--out", type=Path, default=Path(__file__).resolve().parent.parent / "src")
    args = ap.parse_args()

    src = args.unihan_dir / UNIHAN
    if not src.exists():
        print(f"gen_readings_table: {src} not found", file=sys.stderr)
        return 2

    kana_readings, pinyin, digest, revision = parse_unihan(src)
    kana_tab, kana_blob, pinyin_tab, pinyin_blob = build_tables(kana_readings, pinyin)
    args.out.mkdir(parents=True, exist_ok=True)
    dest = args.out / "readings_table.rs"
    emit(dest, kana_tab, kana_blob, pinyin_tab, pinyin_blob, digest, revision)

    print(f"gen_readings_table: {len(kana_readings)} characters with a kana reading, "
          f"{len(pinyin)} with a pinyin reading")
    print(f"gen_readings_table: {len(kana_blob)} distinct kana readings, "
          f"{len(pinyin_blob)} distinct pinyin readings")
    print(f"gen_readings_table: wrote {dest}")
    return 0


if __name__ == "__main__":
    sys.exit(main())
