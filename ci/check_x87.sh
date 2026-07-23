#!/bin/sh
# x87 gate (sprint x09, permanent).
#
# Compiles every non-diagnostic test_programs/*.f90 with its declared
# FLAGS at -O2 and -Ofast on an x86_64 host and fails if any x87 mnemonic
# appears. The compiler is SSE-only by design; x87 creeps in through the
# assembler, not the backend — system `as` accepts fld/fst silently.
# Compiler failures, missing assembly, scanner errors, and corpus count
# drift are all gate failures.
#
# Usage: ci/check_x87.sh [path-to-armfortas]
set -eu

isa_gate_name=check_x87
isa_gate_levels="-O2 -Ofast"
isa_gate_level_label="-O2 and -Ofast"
isa_gate_hit_label="x87 mnemonic"

isa_gate_scan() {
    grep -nE '^\s*f(ld|st|xch|add|sub|mul|div|com|sqrt|prem|sin|cos|abs|chs)' "$1"
}

. "$(dirname "$0")/isa_gate_common.sh"
isa_gate_run "$@"
