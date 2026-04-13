#!/usr/bin/env bash
# Performance regression gate for armfortas.
#
# Compiles a set of representative test programs and measures:
#   - Compile time (wall clock)
#   - Binary size
#
# Compares against a baseline file (.benchmarks/baseline.txt).
# If no baseline exists, creates one.
#
# Usage:
#   ./scripts/benchmark_gate.sh           # compare against baseline
#   ./scripts/benchmark_gate.sh --update  # update the baseline
#
# Thresholds:
#   Compile time: fail if >30% slower than baseline
#   Binary size:  fail if >15% larger than baseline

set -euo pipefail
cd "$(git rev-parse --show-toplevel)"

COMPILER="./target/release/armfortas"
BASELINE=".benchmarks/baseline.txt"
PROGRAMS=(
    test_programs/array_bulk_kernels.f90
    test_programs/module_init.f90
    test_programs/two_loops.f90
    test_programs/derived_type_nested.f90
    test_programs/allocatable.f90
)
OPT="-O2"

if [ ! -x "$COMPILER" ]; then
    echo "Build the compiler first: cargo build --release"
    exit 1
fi

mkdir -p .benchmarks
TMPDIR=$(mktemp -d)
trap "rm -rf $TMPDIR" EXIT

compile_and_measure() {
    local src="$1"
    local stem
    stem=$(basename "$src" .f90)
    local binary="$TMPDIR/$stem"

    # Compile and time it
    local start end elapsed
    start=$(python3 -c 'import time; print(time.monotonic())')
    "$COMPILER" "$src" $OPT -o "$binary" 2>/dev/null
    end=$(python3 -c 'import time; print(time.monotonic())')
    elapsed=$(python3 -c "print(f'{$end - $start:.4f}')")

    # Binary size
    local size
    if [ -f "$binary" ]; then
        size=$(stat -f%z "$binary" 2>/dev/null || stat -c%s "$binary" 2>/dev/null || echo 0)
    else
        size=0
    fi

    echo "$stem $elapsed $size"
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

    # Time regression check (30% threshold)
    time_ratio=$(python3 -c "
bt, ct = $base_time, $time
if bt > 0:
    ratio = ct / bt
    print(f'{ratio:.2f}')
else:
    print('1.00')
")
    time_pct=$(python3 -c "print(f'{($time / max($base_time, 0.001) - 1) * 100:.1f}')")

    # Size regression check (15% threshold)
    size_pct=$(python3 -c "
bs, cs = $base_size, $size
if bs > 0:
    print(f'{(cs / bs - 1) * 100:.1f}')
else:
    print('0.0')
")

    status="OK"
    if python3 -c "exit(0 if $time / max($base_time, 0.001) > 1.30 else 1)" 2>/dev/null; then
        status="SLOW"
        FAIL=1
    fi
    if python3 -c "exit(0 if $size / max($base_size, 1) > 1.15 else 1)" 2>/dev/null; then
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
