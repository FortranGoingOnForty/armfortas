# Noted Items

Pre-existing failure surfaced during sprint-gate runs on nomad
(2026-06-10), NOT caused by x00/l00 (reproduces on a trunk+x00 tree):

- `cargo test -p afs-as --test clang_probe_dashboard` is **flaky** on
  nomad (macOS 26.4.1, Apple clang 21.0.0): observed 2-of-3 over serial
  runs on 2026-06-10 (failed on the l00 tree and on a trunk+x00 tree,
  then passed in a full `--no-fail-fast` workspace run). Failing case:
  `ext_byte_ptr` at O2 links but **runs with the wrong value** (`run`
  column `--`; the driver's `read_ext_byte3() != 77` check fails).
  clang 21 emits `.loh AdrpLdrGotLdr` for the adrp/ldr-got/ldrb chain
  (`_ext_bytes@GOTPAGE` → deref → `ldrb w0, [x8, #3]`). Working
  hypothesis: afs-as forwards the LOH with wrong instruction offsets
  and the macOS 26 `ld` applies the optimization to the wrong
  instructions; intermittence suggests layout/address-dependence
  (ASLR, ld section ordering) or a probe-harness race. A flaky
  wrong-value differential is the worst failure class — needs an
  afs-as reduction (assemble nomad's clang-21 .s with afs-as vs system
  `as`, diff the LOH linkedit payload, run both, repeatedly). Not
  caused by x00/l00.

Deferred items from the l00 F2023 inventory (2026-06-10):

- ~~`! FLAGS:` landed in the root harness (run_programs) but is not
  yet consumed by bencch~~ — resolved in x09: `compile_output` applies
  FLAGS through the driver's CLI parser and the capture path refuses
  loudly on FLAGS-carrying fixtures (`bencch/bench/src/compiler.rs`).
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
  `call @lbound` instead of descriptor reads (l00 probe 22); confirmed on
  nomad — links fail with `_lbound` undefined. Needs a reduction
  independent of F2023.
- The runtime format parser accepts unknown edit-descriptor sequences
  without raising an I/O error: `'(at)'` printed untrimmed text,
  `'(lzs, f6.2)'` printed nothing, both exit 0 (nomad, 2026-06-10).
  Bites typo'd formats today; l05 makes unknown descriptors a runtime
  error as part of the AT/LZ work.
- F2023-syntax collisions producing silent wrong answers today (accepted
  and mis-lowered, garbage at runtime): `real :: a([2,3])` (R818),
  `allocate(x([2,3]))` (R937), pointer rank-remapping with array bounds.
  Details in `.docs/audits/f2023-feature-matrix.md`; owned by l01 —
  until then these spellings corrupt silently rather than erroring.

Resolved during x06 (kept as a lesson for x07's parity sweep):

- (FIXED in x06) The x05 naive allocator sized FP spill traffic off the
  instruction suffix, which on conversions describes the GP side:
  `cvtsi2sdl` stored its double def with movss (4 of 8 bytes),
  `cvttsd2sil` loaded its double source with movss. Objects assembled
  and linked fine; the wrong answers only surfaced when x06 made
  binaries runnable (`x05_conversions` printed `0 / 0.750000 / 0` for
  `3 / 3.750000 / -2`). Pinned by `conversion_spill_traffic_uses_fp_width`
  (x86_object_smoke) and the elf_link_e2e CHECK runs. Lesson: gas
  accepting the asm proves nothing about operand widths — only running
  output does.

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

Found during l02 (2026-06-10), pre-existing on trunk, x86_64 only:

- **silent-wrong-answer (x86 backend)**: `if (c)` on a LOGICAL dummy
  takes the wrong branch — the bool load through the pointer vreg
  selects a register-source `movzbl %r10b` (low byte of the ADDRESS)
  instead of a memory-operand load through it. IR is correct
  (`load ptr<bool>` then `load bool`); the defect is in x86 isel or
  the regalloc address-operand metadata for the movzx family (the
  addr_operand_position list covers MovRM/MovMR/Lea only). arm64
  unaffected. Pinned by `test_programs/x07_bool_dummy_branch.f90`
  (`XFAIL(x86_64)`); owned by the x07 parity sweep. Until fixed, any
  x86 code path branching on a by-ref logical is suspect.

Found while unblocking l02's CI (2026-06-10, all pre-existing on trunk):

- **Parser runaway-allocation loop (FIXED with l02)**: the implicit-main
  parser never consumed CONTAINS, so parse_file pushed empty program
  units forever — 55GB resident before being killed manually; on CI it
  ate both macOS jobs' timeouts. Trigger: any bare main with internal
  procedures, reachable once l02's `?` lexing let conditional_8.f90
  parse past its print line. Fixed: implicit mains parse a CONTAINS
  section (F2018 R1401), plus a parse_file progress guard turning any
  future zero-progress unit parse into a clean error. Bare-main
  internal procedures also exposed a name-mangling mismatch (`<main>`
  scope vs `main` lowering prefix) — aligned. Fixture
  `l02_bare_main_contains.f90`.
- Standalone attribute statements are not parsed: `allocatable :: g`
  (allocatable function result, gfortran-dg conditional_8.f90), and
  presumably the pointer/target forms. Parse error today.
- Conditional expression inside a character LEN spec
  (`character(len=(n > 5 ? n : 5))`, conditional_7.f90) is not parsed —
  the len-spec consumes the opening paren before the conditional check.
- DO CONCURRENT index-in-LOCAL locality constraint is not validated:
  conditional_9.f90 is a dg-error test we accept silently (vacuous
  accept; the conditional in it compiles fine).

Found during x08's differential/coverage work (2026-06-11, both
pre-existing on all targets, both now sema-rejected loudly, owned by
l06's intake):

- **Character VALUE dummies** never had copy-in semantics: the callee
  received the caller's storage pointer, so mutation corrupted the
  caller (SEGV on literal actuals). `x08_value_scalars.f90` found it.
  Rejection covers the Fortran-internal convention only — BIND(C)
  c_char VALUE dummies already byte-copy correctly and stay accepted.
- **Character COMMON members** lowered as a pointer slot instead of
  inline bytes; every read came back empty (afs_write_string got
  len 0). The x08 cross-TU COMMON differential found it; the same
  test also caught COMMON blocks emitting strong .data definitions
  per TU on ELF (duplicate-symbol link errors) — fixed with .comm
  emission for uninitialized commons.

Found during x09's determinism sweep (2026-06-11, pre-existing, all
targets):

- **Circular USE segfaults the compiler** when a multifile bundle is
  compiled as a single source: `error_circular_use_direct.f90` and
  `error_circular_use_indirect.f90` SIGSEGV under `--emit-ir` (and
  every other mode) instead of reporting the cycle. The harness never
  sees it because MULTIFILE_LINK splits the bundle and the expected
  "not found" error fires first. Likely unbounded recursion in module
  resolution. Error-path robustness, not a miscompile; owner: next
  frontend sprint that touches module resolution (l07 submodules is
  the natural slot).

Found while writing x10's NaN min/max fixture (2026-06-12, both
targets, long-standing):

- **The F edit descriptor falls back to E-notation**: `print
  '(F6.1)', 4160.0` emits `4.1600000E3` on macOS and ELF alike. The
  corpus's CHECK lines have been written around it, which is why it
  never surfaced. Owner: l05 (F2023 I/O sprint) — implement Fw.d
  editing alongside the AT/LEADING_ZERO work.

Unexplained transient during x10 validation on nomad (2026-06-12):

- **rank_remap_strided_section_copyin.f90 failed (ERROR STOP 5) at
  -O0 on arm64** from one specific build lineage of the compiler —
  three consecutive runs across two separately-invoked builds — then
  vanished: a per-commit bisect over the same range passed everywhere
  including the previously-failing head, and a 20× compile+run stress
  of the rebuilt head produced zero failures and byte-identical asm
  every time. Same source, different binary, deterministic per
  binary. Prime suspect is corrupted rustc incremental state in
  nomad's target dir (dozens of branch switches that day); cargo
  clean applied. NOT reproduced on a clean build anywhere, and CI's
  fresh-checkout macOS jobs never saw it. If it recurs: SAVE THE BAD
  COMPILER BINARY before rebuilding — that artifact is the evidence
  the investigation needs.

Deferred from l03's enumeration type-safety pass (2026-06-12):

- **Enumeration actuals to FUNCTION references are unchecked**: the
  same-type argument-association check covers CALL statements only
  (mirrors l02's conditional-argument CALL-only precedent). An
  enumeration passed to a non-generic function's integer dummy, or
  vice versa, compiles silently; the value is a valid by-ref i32 so
  it reads the ordinal rather than garbage. Owner: l03 follow-up
  audit.
- **C7114 unenforced**: the access-spec on `ENUMERATION TYPE` is
  accepted anywhere, not just in a module specification section.
  Parser-side, one constraint. Owner: l03 follow-up audit.
- **Enumeration components of derived types**: no type_layout field
  representation; a `TYPE(enum)` component falls into the
  unknown-derived path. Owner: l03 follow-up audit; promote to its
  own row if a target project hits it.

Found running the FULL workspace suite on FreeBSD for the first time
(2026-06-12, l03):

- **19 afs-as suites and 1 afs-ld test hard-fail off macOS arm64**:
  the differential corpus (verify_against_system_as, corpus_compat,
  roundtrip, hello_world, ...) assembles with the system `as`, links
  for the Apple target, and runs the binaries — none of it can work
  on a GNU/x86 host, and none of it skips. clang_probe_dashboard got
  its skip gate (afs-as a02dc89); the rest need either the same gate
  or, better, real ELF coverage. Owner: the afs-as x86_64/ELF phase
  of the multi-platform campaign. Until then `cargo test --workspace`
  is macOS-only; the FreeBSD surface is `cargo test -p armfortas`
  plus the armfortas integration suites (all green here).

Found during l04 (2026-06-12):

- **No general intrinsic argument-count checking.** `atan2(1.0)` (one
  arg to a two-arg intrinsic) compiles silently, as does `atan2d(1.0)`.
  armfortas has no arity gate for elemental/transformational
  intrinsics; misuse is caught only if lowering happens to panic. Out
  of scope for l04 (the F2023 trig additions match the existing
  intrinsics' lack of checking by design). Owner: a dedicated
  intrinsic-signature-checking sprint if a target project surfaces it.

- **SYSTEM_CLOCK runtime is not integer-kind aware** (found l04,
  2026-06-12): afs_system_clock writes i64 COUNT (nanoseconds, ~1.7e18)
  and COUNT_MAX (i64::MAX) into the caller's temp, which the lowering
  then truncates to the argument kind. With default integer (kind 4)
  COUNT and COUNT_MAX overflow — COUNT_MAX reads back as -1. gfortran
  picks a rate/max that fits the argument kind (rate 1000, max
  HUGE(kind)). Fix needs the kind threaded into the runtime call (new
  signature or per-kind entry points). l04 delivered the F2023
  argument-kind RESTRICTIONS (validation); this runtime value-range
  fix is separate. Owner: l05 (I/O + runtime) or a dedicated
  system-intrinsics pass.

- **PRINT with a character format inserts spurious spaces** (found
  l04, 2026-06-12): `print '(A,A,A)', 'x[', s(1:1), ']'` emits
  `   x[ a ]` (leading spaces + spaces around the variable-length
  item), while `write(*,'(A,A,A)') ...` emits `x[a]` correctly. PRINT
  with an explicit char format is not honoring it like WRITE does.
  Most fixtures dodge it via the harness's CHECK whitespace
  normalization; bracketed-token output exposes it. Owner: l05 (I/O).

- **No subroutine-as-function diagnostic** (found l04, 2026-06-12):
  `r = system_clock()` and `r = split(s, set, p)` (intrinsic
  subroutines used in function position) both compile silently. The
  l04 doc assumed an existing "not a function" diagnostic to reuse,
  but armfortas has none for any intrinsic subroutine. Adding one only
  for SPLIT/TOKENIZE would be inconsistent. Owner: a dedicated
  intrinsic-signature-checking pass (same owner as the arity gap).

- **TOKENIZE deferred from l04** (2026-06-12): SPLIT shipped; TOKENIZE
  (both forms) is split out as its own focused piece because it writes
  into caller-allocated arrays — the use-after-free risk class the
  string-descriptor design exists to guard. Implementation approach
  scouted:
  - Form 2, `CALL TOKENIZE(STRING, SET, FIRST, LAST)`: FIRST/LAST are
    allocatable integer arrays. Get each descriptor via
    array_descriptor_addr (see move_alloc_target precedent in
    intrinsic_sub.rs), allocate with afs_allocate_1d(desc, elem_size,
    ntokens), fill 1-based start/end positions. Subtlety: read the
    element kind (4 vs 8) from the local/descriptor, don't assume i32.
  - Form 1, `CALL TOKENIZE(STRING, SET, TOKENS [, SEPARATOR])`: TOKENS
    is an allocatable deferred-length character array — each element a
    string. Must route per-element storage through the
    afs_assign_char_deferred path, never raw malloc, to keep the
    allocate-before-free invariant. This is the hard part and why it's
    deferred rather than rushed.
  Owner: l04 follow-up (l04a) or fold into l05's I/O/runtime work.

- **SPLIT does not bounds-check POS** (found l04, 2026-06-12):
  gfortran's split_3/split_4 dg-shouldfail tests expect a runtime
  error when POS is out of range (or BACK with POS at the string
  start). armfortas intrinsics don't emit runtime argument bounds
  checks, so these run and exit 0. Kept XFAIL. SELECTED_CHAR_KIND
  (F2003, used by split_2) is also unimplemented (undefined symbol at
  link). Owners: intrinsic runtime-bounds pass / F2003 intrinsic
  backlog respectively.
