#!/usr/bin/env bash
# Build the complete source archive used by GitHub releases and downstream
# packages. GitHub's generated source archives do not contain git-submodule
# contents, so they cannot build the armfortas workspace.

set -euo pipefail

usage() {
    echo "usage: $0 <version|vversion> [output-directory]" >&2
    exit 2
}

[ "$#" -ge 1 ] && [ "$#" -le 2 ] || usage

repo_root=$(git rev-parse --show-toplevel)
cd "$repo_root"

version=${1#v}
if [[ ! "$version" =~ ^[0-9]+\.[0-9]+\.[0-9]+([+-][0-9A-Za-z.-]+)?$ ]]; then
    echo "invalid release version: $1" >&2
    exit 2
fi

manifest_version=$(awk '
    $0 == "[package]" { in_package = 1; next }
    /^\[/ { in_package = 0 }
    in_package && $1 == "version" {
        gsub(/["[:space:]]/, "", $3)
        print $3
        exit
    }
' Cargo.toml)
if [ "$version" != "$manifest_version" ]; then
    echo "release version $version does not match Cargo.toml version $manifest_version" >&2
    exit 1
fi

submodule_status=$(git submodule status --recursive)
while IFS= read -r status; do
    [ -z "$status" ] && continue
    case "$status" in
        " "*) ;;
        *)
            echo "submodule is not initialized at its pinned commit: $status" >&2
            exit 1
            ;;
    esac
done <<< "$submodule_status"

output_dir=${2:-"$repo_root/dist"}
mkdir -p "$output_dir"
output_dir=$(cd "$output_dir" && pwd -P)

prefix="armfortas-$version"
archive="$output_dir/$prefix.tar.gz"
checksum_file="$archive.sha256"
source_epoch=${SOURCE_DATE_EPOCH:-$(git show -s --format=%ct HEAD)}

case "$source_epoch" in
    ''|*[!0-9]*)
        echo "SOURCE_DATE_EPOCH must be a non-negative integer" >&2
        exit 2
        ;;
esac

tmp_base=${TMPDIR:-/tmp}
staging_dir=$(mktemp -d "$tmp_base/armfortas-release.XXXXXX")
cleanup() {
    case "$staging_dir" in
        "$tmp_base"/armfortas-release.*) rm -rf -- "$staging_dir" ;;
        *) echo "refusing to remove unexpected staging path: $staging_dir" >&2 ;;
    esac
}
trap cleanup EXIT

umask 022
bundle_root="$staging_dir/$prefix"
mkdir -p "$bundle_root"

while IFS= read -r -d '' path; do
    case "$path" in
        /*|../*|*/../*|*/..)
            echo "refusing unsafe tracked path: $path" >&2
            exit 1
            ;;
    esac
    if [[ "$path" == *$'\n'* ]]; then
        echo "release archives do not support tracked paths containing newlines" >&2
        exit 1
    fi

    source_path="$repo_root/$path"
    destination="$bundle_root/$path"
    mkdir -p "$(dirname "$destination")"
    if [ -L "$source_path" ]; then
        ln -s "$(readlink "$source_path")" "$destination"
    elif [ -f "$source_path" ]; then
        cp -p "$source_path" "$destination"
    else
        echo "tracked path is missing or unsupported: $path" >&2
        exit 1
    fi
done < <(git ls-files --recurse-submodules -z)

if date -u -r "$source_epoch" +%Y%m%d%H%M.%S >/dev/null 2>&1; then
    touch_stamp=$(date -u -r "$source_epoch" +%Y%m%d%H%M.%S)
else
    touch_stamp=$(date -u -d "@$source_epoch" +%Y%m%d%H%M.%S)
fi
find "$bundle_root" ! -type l -exec touch -t "$touch_stamp" {} +
find "$bundle_root" -type l -exec touch -h -t "$touch_stamp" {} +

(
    cd "$staging_dir"
    find "$prefix" -print | LC_ALL=C sort > archive-files.txt
    tar_bin=${ARMFORTAS_TAR:-tar}
    tar_version=$("$tar_bin" --version 2>/dev/null || true)
    case "$tar_version" in
    *"GNU tar"*)
        COPYFILE_DISABLE=1 "$tar_bin" --no-xattrs --format=ustar \
            --owner=0 --group=0 --numeric-owner \
            --mtime="@$source_epoch" --no-recursion \
            -cf source.tar -T archive-files.txt
        ;;
    bsdtar*)
        COPYFILE_DISABLE=1 "$tar_bin" --no-xattrs --format=ustar \
            --uid 0 --gid 0 --uname root --gname root \
            --no-recursion -cf source.tar -T archive-files.txt
        ;;
    *[Bb]usy[Bb]ox*)
        # BusyBox tar has no xattr, format, owner, or mtime switches. The
        # staged tree already has normalized times; CI containers run as
        # root, and BusyBox does not emit AppleDouble metadata.
        COPYFILE_DISABLE=1 "$tar_bin" --no-recursion \
            -cf source.tar -T archive-files.txt
        ;;
    *)
        echo "unsupported tar implementation: ${tar_version:-unknown}" >&2
        exit 1
        ;;
    esac
    gzip -n -9 -c source.tar > "$archive"
)

if command -v sha256sum >/dev/null 2>&1; then
    checksum=$(sha256sum "$archive" | awk '{print $1}')
else
    checksum=$(shasum -a 256 "$archive" | awk '{print $1}')
fi
printf '%s  %s\n' "$checksum" "$(basename "$archive")" > "$checksum_file"

printf '%s\n' "$archive"
printf '%s\n' "$checksum_file"
