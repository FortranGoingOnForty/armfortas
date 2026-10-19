#!/usr/bin/env bash
# Performance regression gate for armfortas.
#
# Compiles a set of representative test programs and measures:
#   - Compile time (wall clock)
#   - Binary size
#
# Compares against a per-target baseline file
# (.benchmarks/baseline-<triple>.txt, triple from `armfortas
# --print-target`). Comparison requires an exact committed baseline;
# baseline creation and refresh are reserved for --update. Thresholds
# are shared across targets; the baselines are not.
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

update_baseline=0
case "$#" in
0) ;;
1)
    if [ "$1" != "--update" ]; then
        echo "Usage: $0 [--update]" >&2
        exit 2
    fi
    update_baseline=1
    ;;
*)
    echo "Usage: $0 [--update]" >&2
    exit 2
    ;;
esac

# Repo root via git when available; the FreeBSD CI VM has no git (the
# tree arrives by rsync) and already runs from the root.
cd "$(git rev-parse --show-toplevel 2>/dev/null || pwd)"

COMPILER="${CARGO_TARGET_DIR:-target}/release/armfortas"
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

for fixture in "${PROGRAMS[@]}" "$BSS_SENTINEL"; do
    if [ ! -f "$fixture" ]; then
        echo "FAIL: missing mandatory benchmark fixture: $fixture" >&2
        exit 1
    fi
done

if [ ! -x "$COMPILER" ]; then
    echo "Build the compiler first: cargo build --release"
    exit 1
fi

TRIPLE=$("$COMPILER" --print-target)
# Linux baselines are additionally distro-tagged: NixOS and ubuntu
# link different crt/libgcc/build-id sets, so binary sizes are only
# comparable within a distro (learned when CI's ubuntu binaries came
# out 63-81% larger than the NixOS baseline).
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

if [ "$update_baseline" -eq 0 ] && [ ! -f "$BASELINE" ]; then
    echo "FAIL: no benchmark baseline exists for $TRIPLE${ENV_TAG}" >&2
    echo "Create it explicitly with: $0 --update" >&2
    exit 1
fi

mkdir -p .benchmarks
benchmark_tmp_root=${TMPDIR:-/tmp}
benchmark_tmp=$(mktemp -d "$benchmark_tmp_root/armfortas-benchmark.XXXXXX")
cleanup_benchmark_tmp() {
    case "$benchmark_tmp" in
    "$benchmark_tmp_root"/armfortas-benchmark.*)
        rm -rf -- "$benchmark_tmp"
        ;;
    *)
        echo "REFUSING to clean unexpected benchmark path: $benchmark_tmp" >&2
        return 1
        ;;
    esac
}
trap cleanup_benchmark_tmp EXIT

results_file="$benchmark_tmp/results.txt"
expected_names_file="$benchmark_tmp/expected-names.txt"
baseline_names_file="$benchmark_tmp/baseline-names.txt"
: > "$results_file"
: > "$expected_names_file"

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
    local binary="$benchmark_tmp/$stem"

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

    local binary="$benchmark_tmp/bss-size-guard"
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
for prog in "${PROGRAMS[@]}"; do
    result=$(compile_and_measure "$prog")
    echo "  $result"
    printf '%s\n' "$result" >> "$results_file"
    basename "$prog" .f90 >> "$expected_names_file"
done
check_bss_size_guard

if [ "$update_baseline" -eq 1 ]; then
    cp "$results_file" "$BASELINE"
    echo "Baseline updated: $BASELINE"
    exit 0
fi

if ! awk -v names_file="$baseline_names_file" '
    NF == 0 { next }
    NF != 3 {
        printf "FAIL: malformed benchmark baseline line %d: expected 3 fields\n", NR > "/dev/stderr"
        bad = 1
        next
    }
    $1 !~ /^[A-Za-z0-9_.-]+$/ || $2 !~ /^[0-9]+([.][0-9]+)?$/ || $3 !~ /^[0-9]+$/ {
        printf "FAIL: malformed benchmark baseline line %d\n", NR > "/dev/stderr"
        bad = 1
        next
    }
    seen[$1]++ {
        printf "FAIL: duplicate benchmark baseline identity: %s\n", $1 > "/dev/stderr"
        bad = 1
        next
    }
    { print $1 > names_file }
    END { exit bad }
' "$BASELINE"; then
    exit 1
fi

if ! cmp -s "$expected_names_file" "$baseline_names_file"; then
    echo "FAIL: benchmark baseline identities do not exactly match the benchmark suite" >&2
    diff -u "$expected_names_file" "$baseline_names_file" >&2 || true
    exit 1
fi

if [ ! -s "$baseline_names_file" ]; then
    echo "FAIL: benchmark baseline contains no identities" >&2
    exit 1
fi

# Compare against baseline
echo ""
echo "Comparing against baseline..."
FAIL=0
while IFS=' ' read -r name time size; do
    [ -z "$name" ] && continue
    baseline_line=$(awk -v wanted="$name" '$1 == wanted { print; exit }' "$BASELINE")
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
done < "$results_file"

if [ $FAIL -ne 0 ]; then
    echo ""
    echo "FAIL: performance regression detected"
    exit 1
else
    echo ""
    echo "PASS: no regressions"
fi
