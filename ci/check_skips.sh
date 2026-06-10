#!/bin/sh
# Skip-count gate (sprint x01).
#
# Usage: ci/check_skips.sh <test-log> <profile>
#
# Profiles:
#   macos     — asserts ZERO HARNESS_SKIP lines. A skip creeping onto
#               the primary platform fails CI.
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
expected_list="$(dirname "$0")/expected_skips_posix-elf.txt"
if [ "$profile" = "posix-elf-musl" ]; then
    merged=$(mktemp)
    cat "$expected_list" "$(dirname "$0")/expected_skips_posix-elf-musl-extra.txt" > "$merged"
    expected_list="$merged"
    profile="posix-elf"
fi

skips=$(grep -c '^HARNESS_SKIP ' "$log" || true)

case "$profile" in
macos)
    # x03+: a small closed set of ELF-host-only suites may skip on
    # macOS; anything outside the list failing to run is the regression
    # this gate exists to catch.
    macos_list="$(dirname "$0")/expected_skips_macos.txt"
    status=0
    grep '^HARNESS_SKIP ' "$log" | sed 's/^HARNESS_SKIP suite=\([^ ]*\) .*/\1/' | sort -u |
    while IFS= read -r seen; do
        if ! grep -qx "$seen" "$macos_list"; then
            echo "check_skips: unexpected suite '$seen' skipped on macOS" >&2
            exit 1
        fi
    done || status=1
    [ "$status" -eq 0 ] && echo "check_skips: macOS profile clean ($skips expected skip lines)"
    exit "$status"
    ;;
posix-elf)
    if [ "$skips" -eq 0 ]; then
        echo "check_skips: no HARNESS_SKIP lines at all — gated suites ran or vanished silently" >&2
        exit 1
    fi
    # Every skip line must have count=<integer >= 1>.
    bad_counts=$(grep '^HARNESS_SKIP ' "$log" | grep -cv ' count=[1-9][0-9]* ' || true)
    if [ "$bad_counts" -ne 0 ]; then
        echo "check_skips: $bad_counts skip line(s) with missing/zero count:" >&2
        grep '^HARNESS_SKIP ' "$log" | grep -v ' count=[1-9][0-9]* ' >&2
        exit 1
    fi
    # Each expected suite must appear at least once.
    status=0
    while IFS= read -r suite; do
        case "$suite" in ''|'#'*) continue ;; esac
        if ! grep -q "^HARNESS_SKIP suite=$suite " "$log"; then
            echo "check_skips: expected suite '$suite' emitted no HARNESS_SKIP line" >&2
            status=1
        fi
    done < "$expected_list"
    # No suite outside the expected list may skip.
    grep '^HARNESS_SKIP ' "$log" | sed 's/^HARNESS_SKIP suite=\([^ ]*\) .*/\1/' | sort -u |
    while IFS= read -r seen; do
        if ! grep -qx "$seen" "$expected_list"; then
            echo "check_skips: unexpected suite '$seen' skipped (not in $expected_list)" >&2
            exit 1
        fi
    done || status=1
    [ "$status" -eq 0 ] && echo "check_skips: posix-elf profile clean ($skips skip lines)"
    exit "$status"
    ;;
*)
    echo "check_skips: unknown profile '$profile' (macos | posix-elf | posix-elf-musl)" >&2
    exit 2
    ;;
esac
