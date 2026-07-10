#!/bin/sh
# x87 gate (sprint x09, permanent).
#
# Compiles every test_programs/*.f90 with -S at -O2 and -Ofast on an
# x86_64 host and fails if any x87 mnemonic appears. The compiler is
# SSE-only by design; x87 creeps in through the assembler, not the
# backend — system `as` accepts fld/fst silently, so nothing else
# fails loudly if isel ever regresses.
#
# Usage: ci/check_x87.sh [path-to-armfortas]
set -eu

afs="${1:-${CARGO_TARGET_DIR:-target}/debug/armfortas}"
if [ ! -x "$afs" ]; then
    echo "check_x87: compiler not found at $afs" >&2
    exit 2
fi

tmpdir=$(mktemp -d)
trap 'rm -rf "$tmpdir"' EXIT

hits=0
checked=0
for lvl in -O2 -Ofast; do
    for f in test_programs/*.f90; do
        b=$(basename "$f" .f90)
        out="$tmpdir/$b$lvl.s"
        # Programs that don't compile are someone else's gate.
        "$afs" -S "$lvl" "$f" -o "$out" 2>/dev/null || continue
        checked=$((checked + 1))
        if grep -nE '^\s*f(ld|st|xch|add|sub|mul|div|com|sqrt|prem|sin|cos|abs|chs)' "$out"; then
            echo "check_x87: x87 mnemonic in $b at $lvl" >&2
            hits=$((hits + 1))
        fi
    done
done

if [ "$hits" -ne 0 ]; then
    echo "check_x87: $hits file(s) contain x87 instructions" >&2
    exit 1
fi
echo "check_x87: clean ($checked compilations, -O2 and -Ofast)"
