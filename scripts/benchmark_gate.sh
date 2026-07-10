#!/usr/bin/env bash
# Performance regression gate for armfortas.
#
# Compiles a set of representative test programs and measures:
#   - Compile time (wall clock)
#   - Binary size
#
# Compares against a per-target baseline file
# (.benchmarks/baseline-<triple>.txt, triple from `armfortas
# --print-target`). If no baseline exists, creates one. Thresholds
# are shared across targets; the baselines are not (x10).
#
# Usage:
#   ./scripts/benchmark_gate.sh           # compare against baseline
#   ./scripts/benchmark_gate.sh --update  # update the baseline
#
# Thresholds:
#   Compile time: fail if >30% slower than baseline
#   Binary size:  fail if >15% larger than baseline
#
# BENCH_SKIP_TIME=1 skips the compile-time comparison (still recorded).
# CI sets it: committed time baselines come from the fleet machines
# (dorado/hasu/nomad) and wall-clock is not comparable across hosts;
# binary size is, so size gates everywhere.

set -euo pipefail
# Repo root via git when available; the FreeBSD CI VM has no git (the
# tree arrives by rsync) and already runs from the root.
cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

COMPILER="./target/release/armfortas"
PROGRAMS=(
    test_programs/array_bulk_kernels.f90
    test_programs/module_init.f90
    test_programs/two_loops.f90
    test_programs/derived_type_nested.f90
    test_programs/allocatable.f90
)
OPT="-O2"
BSS_SENTINEL="test_programs/ar6_bss_module_data.f90"
BSS_SENTINEL_MAX_BYTES=$((32 * 1024 * 1024))

if [ ! -x "$COMPILER" ]; then
    echo "Build the compiler first: cargo build --release"
    exit 1
fi

TRIPLE=$("$COMPILER" --print-target)
# Linux baselines are additionally distro-tagged: NixOS and ubuntu
# link different crt/libgcc/build-id sets, so binary sizes are only
# comparable within a distro (learned when CI's ubuntu binaries came
# out 63-81% larger than the NixOS baseline). An environment without
# a committed baseline bootstraps one and passes — commit it to arm
# the gate there.
ENV_TAG=""
case "$TRIPLE" in
*linux*)
    if [ -r /etc/os-release ]; then
        ENV_TAG="-$(. /etc/os-release && echo "$ID")"
    fi
    ;;
esac
BASELINE=".benchmarks/baseline-${TRIPLE}${ENV_TAG}.txt"
echo "Target: $TRIPLE (baseline: $BASELINE)"

mkdir -p .benchmarks
TMPDIR=$(mktemp -d)
trap "rm -rf $TMPDIR" EXIT

# Monotonic-ish seconds: python3 where present (the fleet boxes),
# whole-second date elsewhere (the CI VM, which gates size only).
now_s() {
    if command -v python3 >/dev/null 2>&1; then
        python3 -c 'import time; print(f"{time.monotonic():.4f}")'
    else
        date +%s
    fi
}

compile_and_measure() {
    local src="$1"
    local stem
    stem=$(basename "$src" .f90)
    local binary="$TMPDIR/$stem"

    # Compile and time it
    local start end elapsed
    start=$(now_s)
    "$COMPILER" "$src" $OPT -o "$binary" 2>/dev/null
    end=$(now_s)
    elapsed=$(awk -v a="$start" -v b="$end" 'BEGIN { printf "%.4f", b - a }')

    # Binary size
    local size
    if [ -f "$binary" ]; then
        size=$(stat -f%z "$binary" 2>/dev/null || stat -c%s "$binary" 2>/dev/null || echo 0)
    else
        size=0
    fi

    echo "$stem $elapsed $size"
}

check_bss_size_guard() {
    local src="$BSS_SENTINEL"
    if [ ! -f "$src" ]; then
        echo "FAIL: missing BSS size sentinel fixture: $src"
        exit 1
    fi

    local binary="$TMPDIR/bss-size-guard"
    "$COMPILER" "$src" $OPT -o "$binary" 2>/dev/null

    local size
    size=$(stat -f%z "$binary" 2>/dev/null || stat -c%s "$binary" 2>/dev/null || echo 0)
    echo "  bss-size-guard $size (limit $BSS_SENTINEL_MAX_BYTES)"

    if [ "$size" -gt "$BSS_SENTINEL_MAX_BYTES" ]; then
        echo ""
        echo "FAIL: uninitialized module data bloated the linked binary"
        exit 1
    fi
}

echo "Benchmarking ${#PROGRAMS[@]} programs at $OPT..."
RESULTS=""
for prog in "${PROGRAMS[@]}"; do
    if [ ! -f "$prog" ]; then
        echo "  SKIP: $prog (not found)"
        continue
    fi
    result=$(compile_and_measure "$prog")
    echo "  $result"
    RESULTS="$RESULTS$result"$'\n'
done
check_bss_size_guard

if [ "${1:-}" = "--update" ]; then
    echo "$RESULTS" > "$BASELINE"
    echo "Baseline updated: $BASELINE"
    exit 0
fi

if [ ! -f "$BASELINE" ]; then
    echo "$RESULTS" > "$BASELINE"
    echo "No baseline found — created: $BASELINE"
    echo "Run again to compare."
    exit 0
fi

# Compare against baseline
echo ""
echo "Comparing against baseline..."
FAIL=0
while IFS=' ' read -r name time size; do
    [ -z "$name" ] && continue
    baseline_line=$(grep "^$name " "$BASELINE" 2>/dev/null || true)
    if [ -z "$baseline_line" ]; then
        echo "  $name: NEW (no baseline)"
        continue
    fi
    base_time=$(echo "$baseline_line" | awk '{print $2}')
    base_size=$(echo "$baseline_line" | awk '{print $3}')

    # All threshold math in awk: the CI VM has no python3.
    time_pct=$(awk -v b="$base_time" -v c="$time" \
        'BEGIN { if (b < 0.001) b = 0.001; printf "%.1f", (c / b - 1) * 100 }')
    size_pct=$(awk -v b="$base_size" -v c="$size" \
        'BEGIN { if (b < 1) b = 1; printf "%.1f", (c / b - 1) * 100 }')

    status="OK"
    if [ "${BENCH_SKIP_TIME:-0}" != "1" ] \
        && [ "$(awk -v b="$base_time" -v c="$time" \
            'BEGIN { if (b < 0.001) b = 0.001; print (c / b > 1.30) ? 1 : 0 }')" = "1" ]; then
        status="SLOW"
        FAIL=1
    fi
    if [ "$(awk -v b="$base_size" -v c="$size" \
        'BEGIN { if (b < 1) b = 1; print (c / b > 1.15) ? 1 : 0 }')" = "1" ]; then
        status="BLOAT"
        FAIL=1
    fi

    echo "  $name: time ${time_pct}% size ${size_pct}% [$status]"
done <<< "$RESULTS"

if [ $FAIL -ne 0 ]; then
    echo ""
    echo "FAIL: performance regression detected"
    exit 1
else
    echo ""
    echo "PASS: no regressions"
fi
