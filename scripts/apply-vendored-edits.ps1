# Re-applies the copy-time transforms to a vendored rgx-core module.
#
# Usage:
#   pwsh -File scripts/apply-vendored-edits.ps1 -Files a.rs,b.rs [-WhatIf]
#
# Why this exists: upstream is the source of truth, so a fix lands there and the
# file is re-copied. Re-doing the copy-time edits by hand each time is how
# they get forgotten, and a forgotten `alloc::` path is a build error at
# best. This makes the transform mechanical and reviewable.
#
# The permitted difference between a upstream source and its rgx copy is exactly
# what this script applies, and nothing else:
#   1. `alloc::` -> `std::` (rgx-core is always std; upstream is no_std + alloc)
#   2. upstream module paths -> rgx-core sibling paths
# Anything else in the copy is drift, and `scripts/check-drift.ps1` is what
# should catch it -- so this script must not *introduce* drift.
#
# -Files takes a comma-separated list, not an array: `pwsh -File` hands
# arguments to a script as plain strings, so `[string[]]` would receive one
# argument containing the commas.
param(
    [Parameter(Mandatory = $true)][string]$Files,
    [switch]$WhatIf
)

# rgx-only divergences from the upstream source, applied after the mechanical
# rewrites. These are not formatting and not `alloc::` -> `std::`; they are
# places where the two projects must genuinely differ, and doing them here
# rather than by hand is what stops a re-copy from silently reverting them.
#
# 1. Feature dispatch. upstream gates SIMD at compile time (`target_feature`),
#    matching b02/simd/mod.rs: a library must not run AVX2 on a CPU without
#    it. rgx ships one binary to arbitrary machines, so it dispatches at
#    runtime the way crates/rgx-core/src/memchr.rs does.
$rgxOnly = @(
    # The braced import. This cannot live in the table above: the `use`
    # branch runs `[regex]::Escape($t.from)`, which escapes the braces a
    # second time and so never matches. A literal Replace is the right tool.
    @{ from = 'use alloc::string::{String, ToString};'; to = 'use std::string::String;' }
    # The upstream module tree is private (`mod nfa;`, `pub(crate) fn parse`),
    # and the rgx copy is flat in `rgx-core/src`, where sibling modules are
    # reached directly. These two lift the visibility the flattening needs;
    # without them the copies do not compile.
    @{ from = 'mod nfa;';      to = 'pub mod nfa;' }
    @{ from = 'mod vm;';       to = 'pub mod vm;' }
    @{ from = 'mod regex;';    to = 'pub mod regex;' }
    @{ from = 'mod syntax;';   to = 'pub mod syntax;' }
    @{ from = 'mod searcher;'; to = 'pub mod searcher;' }
    @{ from = 'pub(crate) ';   to = 'pub ' }
    # Feature dispatch: upstream gates the AVX2 path at compile time (it is a
    # library, and must not run AVX2 on a CPU without it). rgx ships one
    # binary to arbitrary machines, so the copy dispatches at runtime. Both
    # this and the test guards below are re-copy-safe: applying the script
    # twice is a no-op.
    @{
        from = '    #[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]' + "`n" +
               '    {' + "`n" +
               '        // SAFETY: AVX2 is a compile-time guarantee of this build, which is' + "`n" +
               '        // the precondition `simd_candidates` documents.' + "`n" +
               '        return unsafe { simd_candidates(mask, hay) };' + "`n" +
               '    }' + "`n" +
               '    #[allow(unreachable_code)]' + "`n" +
               '    scalar_candidates(mask, hay)'
        to   = '    #[cfg(target_arch = "x86_64")]' + "`n" +
               '    {' + "`n" +
               '        if std::arch::is_x86_feature_detected!("avx2") {' + "`n" +
               '            // SAFETY: guarded by the runtime feature check above, which' + "`n" +
               '            // is `simd_candidates`''s documented precondition.' + "`n" +
               '            return unsafe { simd_candidates(mask, hay) };' + "`n" +
               '        }' + "`n" +
               '    }' + "`n" +
               '    scalar_candidates(mask, hay)'
    }
    @{
        from = '#[cfg(all(target_arch = "x86_64", target_feature = "avx2"))]' + "`n" + '    #[test]'
        to   = '#[cfg(target_arch = "x86_64")]' + "`n" + '    #[test]'
    }
)
$rgxTestGuards = @(
    'simd_matches_scalar_on_random_input',
    'simd_matches_scalar_on_document_shaped_input',
    'a_uniform_haystack_agrees',
    'a_candidate_in_the_high_lane_is_not_lost'
)

$transforms = @(
    # no_std + alloc (upstream) -> std (rgx-core is always std).
    # Each `from` is the whole token after the `use ` prefix, because the
    # pattern below anchors on `use` itself. Keeping the prefix in the
    # pattern and out of the table is what stops a second `use` appearing.
    @{ from = 'alloc::string::String;';                to = 'std::string::String;' }
    @{ from = 'alloc::string::ToString;';             to = 'std::string::ToString;' }
    @{ from = 'alloc::vec::Vec;';                      to = 'std::vec::Vec;' }
    @{ from = 'alloc::vec;';                           to = 'std::vec;' }
    @{ from = 'alloc::boxed::Box;';                    to = 'std::boxed::Box;' }
    @{ from = 'alloc::collections::BTreeMap';          to = 'std::collections::BTreeMap' }
    # `Box` also appears inline in type position (`alloc::boxed::Box<Ast>`),
    # where the `use`-anchored form above does not reach.
    @{ from = '(?m)\balloc::boxed::';                  to = 'std::boxed::' }
    # `format` is also `use`d as a macro, where the macro path is not a
    # `use` line in the way the other forms are.
    @{ from = '(?m)^(\s*use\s+)alloc::format;';        to = '$1std::format;' }
    # These two are not `use` lines, so they get their own anchored forms.
    @{ from = '(?m)\balloc::vec!';                     to = 'vec!' }
    @{ from = '(?m)\balloc::format!';                  to = 'format!' }
    # upstream module paths -> rgx-core sibling paths. The rgx copies are
    # FLATTENED into rgx-core/src, so a `crate::b02::rgx::x` or
    # `super::b02::rgx::x` collapses to the crate root or to `super::`.
    @{ from = '(?m)^(\s*(?:pub )?use\s+[A-Za-z_][A-Za-z0-9_]*)crate::b02::rgx::'; to = '$1crate::' }
    @{ from = '(?m)^(\s*(?:pub )?use\s+super::)b02::rgx::';             to = '$1' }
    @{ from = '(?m)^(\s*(?:pub )?use\s+super::super::)b02::rgx::';     to = '$1' }
    # The same path outside a `use` (test bodies, inline references).
    @{ from = 'crate::b02::rgx::';                                     to = 'crate::' }
    # The upstream module tree is private (`mod nfa;`, `pub(crate) fn parse`),
    # and the rgx copy is flat in `rgx-core/src`, where sibling modules are
    # reached directly. These two lift the visibility the flattening needs;
    # without them the copies do not compile.
    @{ from = '(?m)^mod (nfa|vm|regex|syntax|searcher);';  to = 'pub mod $1;' }
    @{ from = 'pub\(crate\) ';                             to = 'pub ' }
)

$changed = 0
foreach ($f in $Files.Split(',')) {
    if (-not (Test-Path -LiteralPath $f)) {
        Write-Error "no such file: $f"
        continue
    }
    # Read as UTF-8 and write as UTF-8 (no BOM). The default encoding under
    # Windows PowerShell 5.1 is ANSI, which would mangle any non-ASCII byte
    # in a byte-oriented module -- and a test vector of 0xC3 0xA9 is exactly
    # the kind of thing these files carry.
    $c = [IO.File]::ReadAllText($f, [Text.UTF8Encoding]::new($false))
    $before = $c
    foreach ($t in $transforms) {
        # Two shapes: a bare token to swap, matched only in a `use`
        # position, and a pre-anchored pattern (marked with `(?m)`) used
        # for the non-`use` forms. A doc comment or string literal that
        # merely *mentions* `alloc::vec` is quoting the source, and
        # rewriting it would make the provenance header lie about what the
        # copy changed.
        if ($t.from -match '^\(\?m\)') {
            $c = $c -replace $t.from, $t.to
        } else {
            $c = $c -replace "(?m)^(\s*(?:pub )?use\s+)$([regex]::Escape($t.from))", "`$1$($t.to)"
        }
    }
    foreach ($t in $rgxOnly) {
        $c = $c.Replace($t.from, $t.to)
    }
    # The runtime guard a SIMD test needs once the cfg gate is widened. It
    # has to be injected only where it is absent, or a second run of this
    # script stacks a second guard into the same function and the file stops
    # being idempotent -- which is the property that makes the script safe to
    # re-run at all.
    foreach ($name in $rgxTestGuards) {
        $sig = "    fn $name() {"
        $guard = '        if !std::arch::is_x86_feature_detected!("avx2") {'
        if ($c.Contains($sig)) {
            $after = $sig + "`n" + $guard
            if (-not $c.Contains($after)) {
                $c = $c.Replace($sig, $sig + "`n" +
                    $guard + "`n" +
                    '            return;' + "`n" +
                    '        }')
            }
        }
    }
    if ($c -ne $before) {
        $changed++
        if ($WhatIf) {
            Write-Host "would rewrite $f"
        } else {
            [IO.File]::WriteAllText($f, $c, [Text.UTF8Encoding]::new($false))
            Write-Host "rewrote $f"
        }
    } else {
        Write-Host "unchanged $f"
    }
}
Write-Host "$changed file(s) differ from the upstream source's shape"
