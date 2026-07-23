#!/bin/sh
# Prove that each afs-as matrix leg exercised the platform-native differential
# surface. The complete suite intentionally contains tests for both Mach-O and
# ELF, so off-platform skips are expected; a skip for the current leg's own
# toolchain is not.
#
# This is deliberately a prerequisite gate, not a complete skip-manifest
# validator. It does not validate record identities or aggregate counts.
set -eu

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <test-log> <macos-arm64|linux-x86_64>" >&2
    exit 2
fi

log=$1
profile=$2

if [ ! -r "$log" ]; then
    echo "check_afs_as_skips: cannot read test log '$log'" >&2
    exit 2
fi

case "$profile" in
macos-arm64 | linux-x86_64)
    ;;
*)
    echo "check_afs_as_skips: unknown profile '$profile' (macos-arm64 | linux-x86_64)" >&2
    exit 2
    ;;
esac

# Every current matrix leg has suites for the other object format. Seeing their
# records proves that `--nocapture` survived the workflow and that skip evidence
# did not disappear behind libtest's successful status.
if ! grep -q '^HARNESS_SKIP ' "$log"; then
    echo "check_afs_as_skips: no HARNESS_SKIP records; output was captured or platform suites vanished" >&2
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
    # Covers both the standard HARNESS_SKIP wording ("needs") and the legacy
    # clang dashboard wording ("requires").
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

if [ "$status" -eq 0 ]; then
    skips=$(grep -c '^HARNESS_SKIP ' "$log")
    echo "check_afs_as_skips: $profile native coverage clean ($skips off-platform skip records visible)"
fi
exit "$status"
