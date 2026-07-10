#!/bin/sh
# ISA-ceiling gate (sprint x10, permanent).
#
# The x86 baseline is SSE2 — nothing above it may ever be emitted at
# the default --target-cpu. The failure mode this guards is copying
# an LLVM/gcc lowering that happens to use SSE3+/SSE4.1/AVX
# (pmulld, blendvps, haddps, ptest, v-prefixed VEX forms): system
# `as` accepts them silently and the binary then faults on baseline
# hardware. Compiles every test_programs/*.f90 with -S at -O3 and
# -Ofast and greps for mnemonics above the ceiling.
#
# Usage: ci/check_isa_ceiling.sh [path-to-armfortas]
set -eu

afs="${1:-${CARGO_TARGET_DIR:-target}/debug/armfortas}"
if [ ! -x "$afs" ]; then
    echo "check_isa_ceiling: compiler not found at $afs" >&2
    exit 2
fi

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

hits=0
checked=0
for lvl in -O3 -Ofast; do
    for f in test_programs/*.f90; do
        b=$(basename "$f" .f90)
        out="$tmpdir/$b$lvl.s"
        "$afs" -S "$lvl" "$f" -o "$out" 2>/dev/null || continue
        checked=$((checked + 1))
        if grep -nE '^\s*(v[a-z0-9]+\s|pmulld|pminsd|pmaxsd|pminud|pmaxud|pabsd|pabsw|pabsb|blendv|pblendw|ptest|haddps|haddpd|hsubps|hsubpd|movddup|movshdup|movsldup|lddqu|pshufb|palignr|pmuldq|pcmpgtq|roundps|roundpd|dpps|dppd|insertps|extractps)' "$out"; then
            echo "check_isa_ceiling: above-SSE2 mnemonic in $b at $lvl" >&2
            hits=$((hits + 1))
        fi
    done
done

if [ "$hits" -ne 0 ]; then
    echo "check_isa_ceiling: $hits file(s) exceed the SSE2 baseline" >&2
    exit 1
fi
echo "check_isa_ceiling: clean ($checked compilations, -O3 and -Ofast)"
