#!/bin/sh
# Exact skip-manifest gate (sprint x01, hardened by AR37).
#
# Usage: ci/check_skips.sh <test-log> <profile>
#
# Profiles:
#   macos            — exact records in expected_skips_macos.txt
#   posix-elf         — exact records in expected_skips_posix-elf.txt
#   posix-elf-musl    — posix-elf plus exact records in
#                       expected_skips_posix-elf-musl-extra.txt
#
# Manifest rows are: <suite> <test> <positive-count>. The comparison is an
# exact multiset comparison: missing records, unexpected test identities,
# changed counts, and duplicates all fail.
set -eu

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <test-log> <macos|posix-elf|posix-elf-musl>" >&2
    exit 2
fi

log="$1"
profile="$2"
script_dir=$(dirname "$0")
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' 0 HUP INT TERM

if [ ! -r "$log" ]; then
    echo "check_skips: cannot read test log '$log'" >&2
    exit 2
fi

case "$profile" in
macos)
    manifest_input="$script_dir/expected_skips_macos.txt"
    ;;
posix-elf)
    manifest_input="$script_dir/expected_skips_posix-elf.txt"
    ;;
posix-elf-musl)
    manifest_input="$tmp_dir/manifest-input"
    cat "$script_dir/expected_skips_posix-elf.txt" \
        "$script_dir/expected_skips_posix-elf-musl-extra.txt" > "$manifest_input"
    ;;
*)
    echo "check_skips: unknown profile '$profile' (macos | posix-elf | posix-elf-musl)" >&2
    exit 2
    ;;
esac

expected_unsorted="$tmp_dir/expected-unsorted"
while IFS=' ' read -r suite test count extra; do
    case "$suite" in ''|'#'*) continue ;; esac

    if [ -n "${extra:-}" ] || [ -z "${test:-}" ] || [ -z "${count:-}" ]; then
        echo "check_skips: malformed manifest row in $manifest_input: '$suite ${test:-} ${count:-} ${extra:-}'" >&2
        exit 2
    fi
    case "$count" in
        ''|0|*[!0-9]*)
            echo "check_skips: manifest has non-positive count for suite=$suite test=$test count=$count" >&2
            exit 2
            ;;
    esac
    printf '%s %s %s\n' "$suite" "$test" "$count"
done < "$manifest_input" > "$expected_unsorted"

expected="$tmp_dir/expected"
LC_ALL=C sort "$expected_unsorted" > "$expected"
expected_duplicates="$tmp_dir/expected-duplicates"
uniq -d "$expected" > "$expected_duplicates"
if [ -s "$expected_duplicates" ]; then
    echo "check_skips: duplicate rows in expected manifest:" >&2
    awk '{printf "  suite=%s test=%s count=%s\n", $1, $2, $3}' "$expected_duplicates" >&2
    exit 2
fi

markers="$tmp_dir/markers"
grep -o 'HARNESS_SKIP ' "$log" > "$markers" || true
if [ ! -s "$markers" ]; then
    echo "check_skips: no HARNESS_SKIP records at all — gated tests ran or vanished silently" >&2
    exit 1
fi

observed_raw="$tmp_dir/observed-raw"
grep -o 'HARNESS_SKIP suite=[^[:space:]]* test=[^[:space:]]* count=[^[:space:]]*' "$log" \
    > "$observed_raw" || true
marker_count=$(wc -l < "$markers" | tr -d ' ')
record_count=$(wc -l < "$observed_raw" | tr -d ' ')
if [ "$record_count" -ne "$marker_count" ]; then
    echo "check_skips: malformed HARNESS_SKIP record(s): found $marker_count marker(s), parsed $record_count" >&2
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
        echo "check_skips: malformed HARNESS_SKIP record: '$marker $suite_field $test_field $count_field ${extra:-}'" >&2
        exit 1
    fi
    case "$count" in
        ''|0|*[!0-9]*)
            echo "check_skips: non-positive skip count: suite=$suite test=$test count=$count" >&2
            exit 1
            ;;
    esac
    printf '%s %s %s\n' "$suite" "$test" "$count"
done < "$observed_raw" > "$observed_unsorted"

observed="$tmp_dir/observed"
LC_ALL=C sort "$observed_unsorted" > "$observed"
observed_duplicates="$tmp_dir/observed-duplicates"
uniq -d "$observed" > "$observed_duplicates"
if [ -s "$observed_duplicates" ]; then
    echo "check_skips: duplicate HARNESS_SKIP records:" >&2
    awk '{printf "  suite=%s test=%s count=%s\n", $1, $2, $3}' "$observed_duplicates" >&2
    exit 1
fi

if ! cmp -s "$expected" "$observed"; then
    echo "check_skips: $profile skip manifest mismatch" >&2
    missing="$tmp_dir/missing"
    unexpected="$tmp_dir/unexpected"
    comm -23 "$expected" "$observed" > "$missing"
    comm -13 "$expected" "$observed" > "$unexpected"
    if [ -s "$missing" ]; then
        echo "check_skips: missing records:" >&2
        awk '{printf "  suite=%s test=%s count=%s\n", $1, $2, $3}' "$missing" >&2
    fi
    if [ -s "$unexpected" ]; then
        echo "check_skips: unexpected records:" >&2
        awk '{printf "  suite=%s test=%s count=%s\n", $1, $2, $3}' "$unexpected" >&2
    fi
    exit 1
fi

records=$(wc -l < "$observed" | tr -d ' ')
cases=$(awk '{total += $3} END {print total + 0}' "$observed")
echo "check_skips: $profile profile clean ($records exact records, $cases skipped cases)"
