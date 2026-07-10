#!/bin/sh
# Run a command with combined output captured to a log, replay the log, and
# preserve the command's exit status. This is portable to FreeBSD /bin/sh,
# where relying on a pipeline without pipefail can make CI false-green.
set -u

if [ "$#" -lt 2 ]; then
    echo "usage: $0 <log-path> <command> [args...]" >&2
    exit 2
fi

log=$1
shift

status=0
"$@" >"$log" 2>&1 || status=$?
cat "$log"
exit "$status"
