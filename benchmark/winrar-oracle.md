# rgx: rar-codec vs the WinRAR black-box oracle

One question, one suite: **do the bytes `crates/rar-codec` produces match the
bytes the genuine `UnRAR.exe` produces?** Two oracles, complementary by
construction:

| Oracle | Builder | Rows | What it covers |
|---|---|---|---|
| `C:\Users\danalec\Documents\src\winrar-oracle` | `Rar.exe` 7.20 x64 | 43 | the RAR5 matrix (its 3 RAR4 cases skipped at build time — RAR 7.x has no `-ma4`) |
| `C:\Users\danalec\Documents\src\winrar-oracle-rar4` | `Rar.exe` 6.24 x64 | 48 | the same matrix **plus** `rar4_*` and `ppmd_*` |

Both extract their goldens with the same genuine `UnRAR.exe` 7.20 x64
(`winrar-x64-latest\UnRAR.exe`, sha256 `e785a0c0…46542`). Ground truth is the
binary, not a reimplementation.

- **Both oracles proven before any decoder result.**
  `verify_oracle.py` reports `31 case(s) verified, 3 skipped, 0 failed` for
  the 7.20 oracle and `36 case(s) verified, 0 skipped, 0 failed` for the 6.24
  one. A failure below is therefore a decoder failure, not an oracle failure.
- **Runner:** `crates/rar-codec/tests/winrar_oracle.rs`, gated on
  `RGX_WINRAR_ORACLE` / `RGX_WINRAR_ORACLE_RAR4` (forward) and `RGX_UNRAR`
  (reverse). Unset ⇒ both tests skip and the standard gate stays green —
  verified. The runner's own SHA-256 is checked against the FIPS 180-4
  vectors before it compares anything.
- **Verdict.** 7.20 oracle: **39 pass / 0 fail / 4 unsupported**.
  6.24 oracle: **44 pass / 0 fail / 4 unsupported** — including every RAR4
  and PPMd row. Reverse writer check: **5/5**. Every archive SHA-256 matched.

Since the first report (29 pass / 10 fail) one rar-codec defect was fixed:
an empty RAR5 Huffman sub-table is legal (`ldd`/`rd` all-zero when a block
never draws those codes) and `huffman.rs` refused it as malformed. That fix
is what moved the 7.20 oracle to 39/43 with `fail=0`; its history lives in
the fix commit, not in this table.

---

## 1. Forward, 7.20 oracle: 43 golden rows

All RAR5. 39 rows extract byte-identical, 0 fail, 4 unsupported — the
encryption matrix (`enc_data`, `enc_header`, `enc_best`, `enc_data2`).
rar-codec has no decryption at all, by design; the refusal surfaces differ
(data-only encryption is `Unsupported("encrypted payload")`, header
encryption is unreadable headers and surfaces as a parse error at
`Archive::open`) and both are the same missing feature. The header-encryption
error name is misleading — a parse error where the honest answer is "this
archive is encrypted" — a reporting gap, not a wrong answer.

## 2. RAR4: the gap is closed

**`-ma4` works.** `Rar.exe` 6.24 x64 (`winrar-oracle\winrar-x64-624\Rar.exe`,
sha256 `a9cec009…1ea21`) still carries the archive-format switch RAR 7.x
removed. `build_oracle.py` probes `-ma4` itself; against 6.24 the probe
succeeds, `stats.rar4_supported` is `true`, and the build produced 36
archives with **0 skipped** (the 7.20 build of the same script recorded the
three cases as `status: "skipped"`). The builder was run from a copy in the
6.24 oracle directory (extended with the two PPMd cases of §3); the 7.20
oracle's files were not touched. The two oracles are complementary — the
RAR5 rows of the 6.24 oracle were encoded by 6.24, whose internals differ
from 7.20's, so they are extra coverage; RAR5 authority stays with the 7.20
oracle.

Per-case, decoded by rar-codec against the UnRAR 7.20 goldens:

| Case | Switches | Archive sha256 (prefix) | Packed | Status |
|---|---|---|---|---|
| `rar4_normal` | `-ma4 -m3` | `ba969f46…1c09b6` | 4 937 B (40 000 B text) | **pass** |
| `rar4_best` | `-ma4 -m5` | `49e2d32a…b54c117` | 46 238 B (98 304 B mixed) | **pass** |
| `rar4_store` | `-ma4 -m0` | `b051983a…5186bb8` | 131 072 B (stored) | **pass** |

**3/3 byte-identical.** The path under test is `read.rs::parse_rar4` →
`unpack3.rs` (`-m0` store goes through `write_stored`).

### Method-byte probe — and a correction

The RAR 4.x file-header method byte records the **`-m` level, not the
algorithm module**. Observed (header offset 0x2d in these single-file
archives, matching `parse_rar4_file`'s layout):

| Archive | method byte | packed[0] | module |
|---|---|---|---|
| `rar4_normal` | `0x33` ('3' normal) | `0x08`, MSB 0 | LZ+Huffman |
| `rar4_best` | `0x35` ('5' best) | `0x0c`, MSB 0 | LZ+Huffman |
| `rar4_store` | `0x30` ('0' store) | raw bytes | store |
| `ppmd_text` | `0x33` | `0xe3`, MSB 1 | **PPMd** |
| `ppmd_mixed` | `0x33` | `0xe3`, MSB 1 | **PPMd** |

So `0x33` is not "PPMd": `rar4_normal` and `ppmd_text` carry the **same**
method byte. What distinguishes a PPMd member is the packed block header's
leading 1 bit — `unpack3.rs::parse_codes` reads it after byte alignment
(1 = PPMd block header, 0 = Huffman tables). Any earlier claim that the
file-header method byte alone identifies PPMd is wrong; these hexdumps are
the evidence.

## 3. PPMd probe: end-to-end gap **closed**

RARLAB's own `Rar.txt` (6.24) is the switch map: `-mc[param1][:param2]<module>[+-]`
with modules `A` audio, `C` RGB, `D` delta, `E` x86, `T` text — and
"RAR 5.0 archive format supports only 'D' and 'E' values", so `T` (the PPMd
"text compression") is RAR 4.x only. `-mct` allows RAR to choose PPMd for
suitable data; `-mct+` forces it for all data; `-mc<n>:<m>t+` also sets the
PPMd order and memory.

Switches tried with `Rar.exe` 6.24, all on `-ma4`:

| Switch | Build | PPMd block? |
|---|---|---|
| `-mct+` | ok (text: 3 502 B for 40 000 B) | yes (packed[0] MSB 1) |
| `-mct` (auto) | ok, chose PPMd on text anyway | yes |
| `-mc10:40t+` (order 10, 40 MB) | ok | yes |
| `-mc4t+` (order 4) | ok | yes |

Also checked: `-mct+` **without** `-ma4` builds a RAR5 archive happily —
per `Rar.txt` the `T` module is ignored there, and its members are not
PPMd. Every PPMd archive above was built with `-ma4`; 6.24 cannot create
PPMd any other way.

End-to-end, in the 6.24 oracle (built with `-ma4 -mct+`, goldens extracted
by genuine UnRAR 7.20, `verify_oracle.py` clean):

| Case | Archive sha256 (prefix) | Packed | Status |
|---|---|---|---|
| `ppmd_text` (40 000 B generated English) | `67f398cf…9c258d6e` | 2 750 B | **pass** |
| `ppmd_mixed` (98 304 B zeros/random/text) | `87443f64…9da8a66e` | 45 799 B | **pass** |

**Both byte-identical through `ppmd7.rs`.** The caveat "unverified against
real PPMd-bearing archives" is dropped from `ppmd7.rs` and
`scripts/vendored-sources.tsv` on the strength of these two archives. The
synthetic tests keep their job: allocator floors, rescale and restart are
model invariants the end-to-end archives cannot isolate. Still unimplemented
and refused as `Unsupported`: PPMd *filters* (escape code 3) inside a PPMd
block, and arbitrary RAR VM program filters.

## 4. Reverse: rar-codec's store writer against genuine UnRAR

Five store-only RAR5 archives written by `ArchiveBuilder`, extracted by
genuine UnRAR 7.20: `rev_empty_0b`, `rev_single_1b`, `rev_tiny_2b`,
`rev_multi_mixed` (0/1/2-byte + 40 000 + 65 536), `rev_nested_path`.
**5/5 pass**, every member byte-identical.

## 5. What this suite does not say

- **Nothing about decryption.** 4 rows per oracle refused, by design. On the
  6.24 oracle's `enc_header`/`enc_best` the refusal surfaces as
  `CRC mismatch: rar5 header` (7.20's build: `truncated RAR archive`) —
  same missing feature, different parse weather.
- **Nothing about the RAR 7.x compression algorithm (version 1).** Every
  archive here is `v50`; rar-codec reports version 1 as `Unsupported`.
- **`paths_ep2` is machine-specific** in *both* oracles: `-ep2` stores
  absolute paths, so those rows embed the directory each oracle was built
  into (`…\winrar-oracle\corpus\…` vs `…\winrar-oracle-rar4\corpus\…`).
  Every other case is portable.
- **Two oracles are not two independent encoders.** 6.24's RAR5 rows are
  extra coverage of the same standard, not a second authority.

## 6. Reproducing

```sh
# prove the oracles first — do not run the harness before these pass
python C:\Users\danalec\Documents\src\winrar-oracle\verify_oracle.py ^
    --unrar C:\Users\danalec\Documents\src\winrar-oracle\winrar-x64-latest\UnRAR.exe
python C:\Users\danalec\Documents\src\winrar-oracle\verify_oracle.py ^
    --unrar C:\Users\danalec\Documents\src\winrar-oracle\winrar-x64-latest\UnRAR.exe ^
    --manifest C:\Users\danalec\Documents\src\winrar-oracle-rar4\manifest.json

RGX_WINRAR_ORACLE=C:\Users\danalec\Documents\src\winrar-oracle ^
RGX_WINRAR_ORACLE_RAR4=C:\Users\danalec\Documents\src\winrar-oracle-rar4 ^
RGX_UNRAR=C:\Users\danalec\Documents\src\winrar-oracle\winrar-x64-latest\UnRAR.exe ^
    cargo test --release -p rar-codec --test winrar_oracle -- --nocapture
```

The forward test asserts zero `fail` rows and prints both tables. Unset, the
tests skip and the standard gate is green. The 6.24 oracle is rebuilt by
`winrar-oracle-rar4\build_oracle.py` (a copy of the 7.20 builder extended
with the RAR4 PPMd cases) with `--rar winrar-x64-624\Rar.exe --unrar
winrar-x64-latest\UnRAR.exe`; its `manifest.json` pins the exact `Rar.exe`
command line of every case.
