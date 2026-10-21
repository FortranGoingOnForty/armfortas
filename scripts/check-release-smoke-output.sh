#!/usr/bin/env bash
# Check the complete normalized stream produced by the release smoke binary.

set -euo pipefail

if [ "$#" -ne 2 ]; then
    echo "usage: $0 <stdout-file> <stderr-file>" >&2
    exit 2
fi

stdout_file=$1
stderr_file=$2
for stream_file in "$stdout_file" "$stderr_file"; do
    if [ ! -f "$stream_file" ]; then
        echo "release smoke stream does not exist: $stream_file" >&2
        exit 1
    fi
done

if [ -s "$stderr_file" ]; then
    echo "release smoke produced unexpected stderr" >&2
    exit 1
fi

# List-directed formatting is processor-dependent, so normalize horizontal
# whitespace within the one expected record. Extra records remain observable.
if ! awk '
    {
        line = $0
        gsub(/[[:space:]]+/, " ", line)
        sub(/^ /, "", line)
        sub(/ $/, "", line)
        records++
        if (records != 1 || line != "42") {
            bad = 1
        }
    }
    END { exit (records == 1 && !bad) ? 0 : 1 }
' "$stdout_file"; then
    echo "release smoke produced unexpected normalized stdout" >&2
    exit 1
fi
