#!/usr/bin/env bash
# Verify that a packaged release source archive builds and can compile and run
# a native Fortran program without referring back to the source checkout.

set -euo pipefail

if [ "$#" -ne 1 ]; then
    echo "usage: $0 <armfortas-version.tar.gz>" >&2
    exit 2
fi

archive=$1
if [ ! -f "$archive" ]; then
    echo "release archive does not exist: $archive" >&2
    exit 1
fi
archive_dir=$(cd "$(dirname "$archive")" && pwd -P)
archive="$archive_dir/$(basename "$archive")"
checksum_file="$archive.sha256"
if [ ! -f "$checksum_file" ]; then
    echo "release checksum does not exist: $checksum_file" >&2
    exit 1
fi

archive_name=$(basename "$archive")
case "$archive_name" in
    armfortas-*.tar.gz) version=${archive_name#armfortas-}; version=${version%.tar.gz} ;;
    *) echo "unexpected release archive name: $archive_name" >&2; exit 2 ;;
esac
prefix="armfortas-$version"

(
    cd "$archive_dir"
    if command -v sha256sum >/dev/null 2>&1; then
        sha256sum -c "$(basename "$checksum_file")"
    else
        shasum -a 256 -c "$(basename "$checksum_file")"
    fi
)

tmp_base=${RUNNER_TEMP:-${TMPDIR:-/tmp}}
validation_dir=$(mktemp -d "$tmp_base/armfortas-release-validation.XXXXXX")
cleanup() {
    case "$validation_dir" in
        "$tmp_base"/armfortas-release-validation.*) rm -rf -- "$validation_dir" ;;
        *) echo "refusing to remove unexpected validation path: $validation_dir" >&2 ;;
    esac
}
trap cleanup EXIT

while IFS= read -r entry; do
    case "$entry" in
        "$prefix"|"$prefix"/*) ;;
        *) echo "archive path escapes $prefix: $entry" >&2; exit 1 ;;
    esac
done < <(tar -tzf "$archive")

tar -xzf "$archive" -C "$validation_dir"
source_root="$validation_dir/$prefix"
cd "$source_root"

cargo build --release --locked --bin armfortas --bin afs

target_dir=${CARGO_TARGET_DIR:-target}
case "$target_dir" in
    /*) ;;
    *) target_dir="$source_root/$target_dir" ;;
esac
compiler="$target_dir/release/armfortas"
alias_compiler="$target_dir/release/afs"

"$compiler" --version | grep -F "armfortas $version ("
"$alias_compiler" --version | grep -F "afs $version ("

smoke_dir="$validation_dir/smoke"
mkdir -p "$smoke_dir"
printf '%s\n' \
    'program release_smoke' \
    '  implicit none' \
    '  print *, 42' \
    'end program release_smoke' > "$smoke_dir/hello.f90"
AFS_RUNTIME_CACHE="$validation_dir/runtime-cache" \
    "$compiler" "$smoke_dir/hello.f90" -o "$smoke_dir/hello"
smoke_output=$("$smoke_dir/hello")
case "$smoke_output" in
    *42*) ;;
    *) echo "release compiler produced unexpected output: $smoke_output" >&2; exit 1 ;;
esac

echo "validated $archive_name with $($compiler --print-target)"
