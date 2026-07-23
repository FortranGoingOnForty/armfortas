#!/bin/sh
# ISA-ceiling gate (sprint x10, permanent).
#
# The x86 baseline is SSE2 — nothing above it may ever be emitted at
# the default --target-cpu. The failure mode this guards is copying
# an LLVM/gcc lowering that happens to use SSE3+/SSE4.1/AVX
# (pmulld, blendvps, haddps, ptest, v-prefixed VEX forms): system
# `as` accepts them silently and the binary then faults on baseline
# hardware. Compiles every non-diagnostic test_programs/*.f90 with its
# declared FLAGS at -O3 and -Ofast and validates every emitted instruction
# against the ceiling. GNU as is the instruction-set oracle, avoiding an
# open-ended mnemonic denylist. Compiler failures, missing assembly,
# policy-checker errors, and corpus count drift are all gate failures.
#
# Usage: ci/check_isa_ceiling.sh [path-to-armfortas]
set -eu

isa_gate_name=check_isa_ceiling
isa_gate_levels="-O3 -Ofast"
isa_gate_level_label="-O3 and -Ofast"
isa_gate_hit_label="above-SSE2 mnemonic"
isa_gate_scanner="${AS:-as}"
isa_gate_arch_prelude='.arch generic64
.arch .sse2'
isa_gate_allowed_probe='movsd %xmm0, %xmm1'
isa_gate_forbidden_probe='addsubps %xmm0, %xmm1'

. "$(dirname "$0")/isa_gate_common.sh"
isa_gate_run "$@"
