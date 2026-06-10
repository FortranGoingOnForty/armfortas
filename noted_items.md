# Noted Items

Deferred items from the l00 F2023 inventory (2026-06-10):

- `! FLAGS:` landed in the root harness (run_programs) but is not yet
  consumed by bencch; per the one-dialect rule bencch must either apply
  it or clearly report it unsupported. Today bencch only parses
  `! CHECK:` (`bencch/bench/src/lib.rs:4866`), so a shared fixture with
  FLAGS would compile without its flags and could silently diverge.
- `USE <intrinsic-module>, ONLY: name` does not validate `name`:
  `use iso_fortran_env, only: zzz_not_a_thing` compiles silently.
- Implicit external function calls are accepted in constant contexts:
  `integer, parameter :: lk = selected_logical_kind(8)` compiled to a
  runtime call in a parameter initializer before l04 lands the
  intrinsic. Should be a hard error independent of F2023.
- OPEN/WRITE specifier keywords are not validated against the supported
  set (`open(..., leading_zero='suppress')` accepted with no
  implementation behind it).
- `lbound` on a rank-remapped pointer lowered to an external
  `call @lbound` instead of descriptor reads (l00 probe 22); needs a
  reduction independent of F2023.

Deferred items that came up while finishing Sprint 29.10 cleanup work and
starting the full Sprint 29 audit:

- Audit and harden descriptor-backed `integer(16)` formatted section reads at the
  backend/harness boundary.
  The lowering gap is now closed for allocatable section destinations with real
  runtime bounds/strides, and dedicated fixture-backed audits cover IR plus
  O1+/high-opt runtime behavior, but O0 still exposes the existing large-frame
  slot-addressing backend issue in some cases and the external whole-array
  fixture still returns `exit -1` under `capture_from_path(...Stage::Run...)`
  even though direct CLI O1/O2/O3 runs succeed.
- Parser gap: typed character array constructors using an explicit type-spec
  inside brackets (for example `[character(len=20) :: "a", "b"]`) still fail
  to parse in at least one real-world-style source shape.
  The full-sprint audit tripped this while building the fpm-inspired
  `realworld_suffix_scan.f90` reproducer, which is currently written in a more
  conservative source form instead of the typed constructor spelling.
- Revisit ambitious array/vectorization-style `integer(16)` rewrites only after the
  scalar/runtime ABI surface is fully closed and audited.
- Stdlib sweep provenance: `example_solve_custom` passed as the fpm-linked v47
  binary, but previously aborted in one repacked/direct archive path. A fresh
  v64b stdlib rebuild from current upstream exposed a SIGSEGV in
  `example_solve_custom` and the related linalg iterative solver examples. The
  v65b rebuild after routing indirect branch targets through IP1 clears the
  solver SIGSEGV cluster; keep this note as provenance if the archive-order or
  solver-path discrepancy returns.
- Fortsh smoke regression to verify after the current stdlib drill: the existing
  armfortas-built scratch binary at
  `/private/tmp/fortsh-sprint29.X8P616/bin/fortsh` prints `fortsh 1.7.0` for
  `--version` but aborts on `fortsh -c 'printf ok\n'`; the gfortran-built
  scratch control at `/private/tmp/fortsh-gfortran-sprint29.edpvJT/bin/fortsh`
  executes the same basic `-c` path. A quick LLDB run reports
  `malloc: pointer being freed was not allocated` followed by `SIGABRT`, with
  many malformed-DWARF warnings. Fresh detached rebuild of tracked fortsh HEAD
  `ae2924b` with current `compiler-edges` (`b6a2c83`) does not reproduce the
  abort, but still misbehaves on the `-c` path: `-c false` exits 0,
  `-c 'echo ok; false'` emits no stdout and exits 0, and `echo ok > file`
  fails with `fortsh: : No such file or directory`; the gfortran scratch control
  prints `ok`, preserves exit 1 for `false`, and writes the redirected file.
  Drill current `-c` execution/exit-status behavior before returning to fortsh
  as a compiler acceptance target.
