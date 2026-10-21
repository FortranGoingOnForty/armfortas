#!/bin/sh
# Exact afs-as platform skip-manifest gate.
set -eu

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <test-log> <macos-arm64|linux-x86_64>" >&2
    exit 2
fi

log=$1
profile=$2
script_dir=$(dirname "$0")
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' 0 HUP INT TERM

if [ ! -r "$log" ]; then
    echo "check_afs_as_skips: cannot read test log '$log'" >&2
    exit 2
fi

case "$profile" in
macos-arm64)
    manifest="$script_dir/expected_afs_as_skips_macos-arm64.txt"
    ;;
linux-x86_64)
    manifest="$script_dir/expected_afs_as_skips_linux-x86_64.txt"
    ;;
*)
    echo "check_afs_as_skips: unknown profile '$profile' (macos-arm64 | linux-x86_64)" >&2
    exit 2
    ;;
esac

expected_unsorted="$tmp_dir/expected-unsorted"
while IFS=' ' read -r suite test count extra; do
    case "$suite" in ''|'#'*) continue ;; esac
    if [ -n "${extra:-}" ] || [ -z "${test:-}" ] || [ -z "${count:-}" ]; then
        echo "check_afs_as_skips: malformed manifest row in $manifest: '$suite ${test:-} ${count:-} ${extra:-}'" >&2
        exit 2
    fi
    case "$count" in
        ''|0|*[!0-9]*)
            echo "check_afs_as_skips: manifest has non-positive count for suite=$suite test=$test count=$count" >&2
            exit 2
            ;;
    esac
    printf '%s %s %s\n' "$suite" "$test" "$count"
done < "$manifest" > "$expected_unsorted"

expected="$tmp_dir/expected"
LC_ALL=C sort "$expected_unsorted" > "$expected"
expected_identities="$tmp_dir/expected-identities"
awk '{print $1, $2}' "$expected" > "$expected_identities"
expected_duplicates="$tmp_dir/expected-duplicates"
uniq -d "$expected_identities" > "$expected_duplicates"
if [ -s "$expected_duplicates" ]; then
    echo "check_afs_as_skips: duplicate identities in expected manifest:" >&2
    awk '{printf "  suite=%s test=%s\n", $1, $2}' "$expected_duplicates" >&2
    exit 2
fi

markers="$tmp_dir/markers"
grep -o 'HARNESS_SKIP ' "$log" > "$markers" || true
if [ ! -s "$markers" ]; then
    echo "check_afs_as_skips: no HARNESS_SKIP records; output was captured or platform suites vanished" >&2
    exit 1
fi

observed_raw="$tmp_dir/observed-raw"
grep -o 'HARNESS_SKIP suite=[^[:space:]]* test=[^[:space:]]* count=[^[:space:]]*' "$log" \
    > "$observed_raw" || true
marker_count=$(wc -l < "$markers" | tr -d ' ')
record_count=$(wc -l < "$observed_raw" | tr -d ' ')
if [ "$record_count" -ne "$marker_count" ]; then
    echo "check_afs_as_skips: malformed or interleaved HARNESS_SKIP record(s): found $marker_count marker(s), parsed $record_count" >&2
    grep -Fn 'HARNESS_SKIP ' "$log" >&2 || true
    exit 1
fi

observed_unsorted="$tmp_dir/observed-unsorted"
while IFS=' ' read -r marker suite_field test_field count_field extra; do
    suite=${suite_field#suite=}
    test=${test_field#test=}
    count=${count_field#count=}
    if [ "$marker" != "HARNESS_SKIP" ] \
        || [ "$suite" = "$suite_field" ] \
        || [ "$test" = "$test_field" ] \
        || [ "$count" = "$count_field" ] \
        || [ -n "${extra:-}" ] \
        || [ -z "$suite" ] \
        || [ -z "$test" ]; then
        echo "check_afs_as_skips: malformed HARNESS_SKIP record: '$marker $suite_field $test_field $count_field ${extra:-}'" >&2
        exit 1
    fi
    case "$count" in
        ''|0|*[!0-9]*)
            echo "check_afs_as_skips: non-positive skip count: suite=$suite test=$test count=$count" >&2
            exit 1
            ;;
    esac
    printf '%s %s %s\n' "$suite" "$test" "$count"
done < "$observed_raw" > "$observed_unsorted"

observed="$tmp_dir/observed"
LC_ALL=C sort "$observed_unsorted" > "$observed"
observed_identities="$tmp_dir/observed-identities"
awk '{print $1, $2}' "$observed" > "$observed_identities"
observed_duplicates="$tmp_dir/observed-duplicates"
uniq -d "$observed_identities" > "$observed_duplicates"
if [ -s "$observed_duplicates" ]; then
    echo "check_afs_as_skips: duplicate HARNESS_SKIP identities:" >&2
    awk '{printf "  suite=%s test=%s\n", $1, $2}' "$observed_duplicates" >&2
    exit 1
fi

if ! grep -Fq 'test result: ok.' "$log"; then
    echo "check_afs_as_skips: log has no successful cargo test result" >&2
    exit 1
fi

status=0
reject_reason() {
    reason=$1
    if grep -Fq "$reason" "$log"; then
        echo "check_afs_as_skips: $profile emitted a native-platform prerequisite skip containing '$reason':" >&2
        grep -Fn "$reason" "$log" >&2 || true
        status=1
    fi
}

case "$profile" in
macos-arm64)
    reject_reason "macOS arm64 host toolchain"
    ;;
linux-x86_64)
    reject_reason "ELF smoke needs an ELF host"
    reject_reason "no GNU assembler on this host"
    reject_reason "no readelf on PATH"
    reject_reason "no ld on PATH"
    reject_reason "no system linker on PATH"
    reject_reason "ld is not GNU ld"
    reject_reason "needs Linux GNU ld semantics"
    ;;
esac
if [ "$status" -ne 0 ]; then
    exit "$status"
fi

if ! cmp -s "$expected" "$observed"; then
    echo "check_afs_as_skips: $profile exact skip manifest mismatch" >&2
    missing="$tmp_dir/missing"
    unexpected="$tmp_dir/unexpected"
    comm -23 "$expected" "$observed" > "$missing"
    comm -13 "$expected" "$observed" > "$unexpected"
    if [ -s "$missing" ]; then
        echo "check_afs_as_skips: missing records:" >&2
        awk '{printf "  suite=%s test=%s count=%s\n", $1, $2, $3}' "$missing" >&2
    fi
    if [ -s "$unexpected" ]; then
        echo "check_afs_as_skips: unexpected records:" >&2
        awk '{printf "  suite=%s test=%s count=%s\n", $1, $2, $3}' "$unexpected" >&2
    fi
    exit 1
fi

records=$(wc -l < "$observed" | tr -d ' ')
cases=$(awk '{total += $3} END {print total + 0}' "$observed")
echo "check_afs_as_skips: $profile profile clean ($records exact records, $cases skipped cases)"
