# x86_64 -O0 parity sweep log (x07)

Sweep of `test_programs/` at -O0 on x86_64 ELF hosts, per the x07
sprint doc. Finding IDs here are referenced by
`! XFAIL(x86_64): X64-O0-NNN` annotations in `test_programs/` and the
pairing is enforced by `xfail_findings_cross_check` in
`tests/run_programs.rs`.

## Totals (2026-06-10, corpus 533 entries)

| platform | pass | XFAIL | notes |
|---|---|---|---|
| x86_64-freebsd (dorado) | 513 | 18 | sweep runs in ~25s |
| x86_64-linux-gnu (hasu, NixOS) | 513 | 18 | full suite 120/120 green, ~52s |

Fixed during the sweep rather than annotated (the >20-findings rule —
each was a systemic root cause failing dozens of programs):

- Bare `! ASM_CHECK:`/`! ASM_NOT:` asserted ARM patterns against x86
  text — 252 programs. Harness: unscoped ASM directives now mean
  arm64-macos per the x01 grammar (`HARNESS`).
- `movss/movsd` loads through a pointer held in a vreg lowered the
  pointer as the value (`movss %r10d, %xmm14`, gas reject) — ~100
  programs. Same address-class defect as the bool-load fix: a GP-class
  operand 0 on an FP move is an address (`BACKEND`).
- `.amod` emission sat below the ELF early-return in the driver, so no
  module files were written on ELF — all multifile programs
  (`HARNESS`-adjacent driver bug, category `FRONTEND` by the taxonomy's
  letter; fixed by hoisting emission above the format branch).
- `command_argument_count()` returned -1: std::env::args's argv
  init_array member is dropped when linking the staticlib under a
  non-Rust main; entry wrappers now forward main's (argc, argv) into
  afs_program_init and the runtime stores them (`ABI`/runtime).
- i16 stores panicked (16-bit register names unwired); the bool/i8/i16
  extending-load addressing bug (see x07 sprint doc inventory)
  (`BACKEND`).
- OPT_EQ rules compared against opt levels ELF hosts cannot compile
  until x09; comparisons now skip unsupported levels per
  `native_e2e_level_support` (`HARNESS`).

## Findings

### X64-O0-001 — i128 values are not selected by the x86 backend [FIXED in x08]

Resolved: i128 values are memory-resident (lo, hi) frame-slot pairs
staged through rax:rdx (the arm64 wide-slot model); add/adc, sub/sbb,
neg, compares via the sub/sbb flags idiom, rax:rdx returns, GpPair
args with the SysV revert rule. All 13 programs pass and the
annotations are removed; kept for history:
- Programs (13): integer16_format, integer16_format_read,
  integer16_format_read_arrays, integer16_format_read_sections,
  integer16_format_read_targets, integer16_internal_format,
  integer16_internal_format_read, integer16_internal_format_read_arrays,
  integer16_internal_format_read_sections,
  integer16_internal_format_read_targets, integer16_internal_io,
  integer16_print, integer16_read
- Both ELF platforms: compile error `x05 scope: i128 values deferred`.
- Category: BACKEND. The isel has no i128 register-pair strategy;
  x05 deferred it deliberately (loud error, never wrong answers).
- Owner: x08 — DONE (this entry retained as the finding record).

### X64-O0-002 — indirect calls through procedure pointers [FIXED in x08]
- Programs (3): procedure_dummy_interface_scope_hidden_lengths,
  procedure_pointer_intent_out_parent_default,
  stdlib_hashmaps_tbp_int8_array_dispatch
- Both ELF platforms: compile error `x05 scope: indirect calls deferred`.
- Category: BACKEND/ABI. Call selection only handles direct FuncRefs;
  calling through a value needs `call *%reg` plus the SysV argument
  marshaling it shares with direct calls.
- Owner: x08 — DONE. Indirect targets stage into r11 and call *%r11; argument marshaling is shared with direct calls.

### X64-O0-003 — no register class for by-value array/complex aggregates [FIXED in x08]
- Programs (2): complex_dp_parameter_zero_compare,
  stdlib_math_swap_cdp_default_cmplx_array
- Both ELF platforms: compile error
  `x05 scope: no register class for Array(Float(F64), 2)`.
- Category: ABI. Complex values reaching isel as by-value
  `Array(f64, 2)` need the x04 classifier's SSE-pair treatment wired
  into value classing instead of a vreg class lookup.
- Owner: x08 — DONE. 8-byte by-value arrays ride Gp64 raw bits; 16-byte ones reuse the i128 wide-slot pair machinery.
