#!/bin/sh
# Skip-count gate (sprint x01).
#
# Usage: ci/check_skips.sh <test-log> <profile>
#
# Profiles:
#   macos     — asserts the closed set of platform-only suites in
#               ci/expected_skips_macos.txt. Missing, zero-count, and
#               unexpected skips fail.
#   posix-elf — asserts the gated suites (ci/expected_skips_posix-elf.txt)
#               each emitted at least one HARNESS_SKIP line, every line
#               carries an integer count >= 1, and no suite outside the
#               expected list skipped. Silent skips and count=0 skips
#               both fail.
#   posix-elf-musl — posix-elf plus the suites in
#               ci/expected_skips_posix-elf-musl-extra.txt (x06: the
#               native link gate, which a musl host cannot run until
#               x11). Same strictness, larger expected set.
set -eu

log="$1"
profile="$2"
script_dir=$(dirname "$0")
tmp_dir=$(mktemp -d)
trap 'rm -rf "$tmp_dir"' 0 HUP INT TERM

case "$profile" in
macos)
    expected_list="$script_dir/expected_skips_macos.txt"
    ;;
posix-elf)
    expected_list="$script_dir/expected_skips_posix-elf.txt"
    ;;
posix-elf-musl)
    expected_list="$tmp_dir/expected"
    cat "$script_dir/expected_skips_posix-elf.txt" \
        "$script_dir/expected_skips_posix-elf-musl-extra.txt" > "$expected_list"
    ;;
*)
    echo "check_skips: unknown profile '$profile' (macos | posix-elf | posix-elf-musl)" >&2
    exit 2
    ;;
esac

skips=$(grep -c '^HARNESS_SKIP ' "$log" || true)
if [ "$skips" -eq 0 ]; then
    echo "check_skips: no HARNESS_SKIP lines at all — gated suites ran or vanished silently" >&2
    exit 1
fi

bad_counts=$(grep '^HARNESS_SKIP ' "$log" | grep -cv ' count=[1-9][0-9]* ' || true)
if [ "$bad_counts" -ne 0 ]; then
    echo "check_skips: $bad_counts skip line(s) with missing/zero count:" >&2
    grep '^HARNESS_SKIP ' "$log" | grep -v ' count=[1-9][0-9]* ' >&2
    exit 1
fi

status=0
while IFS= read -r suite; do
    case "$suite" in ''|'#'*) continue ;; esac
    if ! grep -Fq "HARNESS_SKIP suite=$suite " "$log"; then
        echo "check_skips: expected suite '$suite' emitted no HARNESS_SKIP line" >&2
        status=1
    fi
done < "$expected_list"

seen_list="$tmp_dir/seen"
grep '^HARNESS_SKIP ' "$log" | sed 's/^HARNESS_SKIP suite=\([^ ]*\) .*/\1/' | sort -u > "$seen_list"
while IFS= read -r seen; do
    if ! grep -Fqx "$seen" "$expected_list"; then
        echo "check_skips: unexpected suite '$seen' skipped (not in $expected_list)" >&2
        status=1
    fi
done < "$seen_list"

[ "$status" -eq 0 ] && echo "check_skips: $profile profile clean ($skips skip lines)"
exit "$status"
