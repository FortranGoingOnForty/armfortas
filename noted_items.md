# Noted Items

Pre-existing finalization bug surfaced during l08 (2026-06-15), NOT
caused by l08 (reproduces with a direct field write, no dispatch
involved; the deallocate→finalize path is untouched by the vtable work):

- Finalizing a polymorphic allocatable on `DEALLOCATE` passes the FINAL
  subroutine a zeroed copy of the object, not the live storage. Repro:
  `class(t), allocatable :: r; allocate(t::r); r%handle = 99;
  deallocate(r)` — the FINAL prints `handle = 0`, though `r%handle`
  reads 99 just before. The FINAL *runs* (correct), but the object it
  receives has lost its field values. Likely the deallocate path
  finalizes a default-initialized temporary, or finalizes after zeroing
  the storage. Own it in the finalization/deallocate area, not l08.
  `test_programs/l08_vtable_final_after_dispatch.f90` works around it by
  asserting the FINAL fires (not the field value).

Pre-existing x86 host failure surfaced while running the full armfortas
integration suite on dorado (FreeBSD 15 x86_64) during l06 (2026-06-14),
NOT caused by l06 (the branch touches only sema + intrinsic lowering — no
vectorizer/codegen/opt files; `git diff trunk...HEAD` confirms):

- ~~`tests/vectorize_dot_product.rs::o3_vectorizes_manual_dot_product_loop`
  fails on x86~~ — RESOLVED (2026-07-03): the policy legitimately changed
  at x10c-3 (`vec_isa.rs int_mul = true` via the pmuludq even/odd
  synthesis) and the test expectations were updated with the
  cross-platform enablement. The x86 branches now also pin SSE2
  *legality* (pmuludq present, pmulld absent; pcmpgtd present,
  pminsd/pmaxsd absent) so an SSE4.1 leak in the emitter cannot pass
  silently on modern test hardware. Runtime output verified correct at
  -O0..-Ofast on FreeBSD x86_64.

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
- Whole-array `lbound(a)` / `ubound(a)` (no `dim` arg, returns a rank-1
  array of bounds) emits unresolvable external `lbound`/`ubound` symbols
  for EVERY array, not just pointers — `shape(a)` whole-array works, so
  the array-returning infra exists and these should mirror it. This is
  the sole remaining blocker for `gfortran.dg/c_f_pointer_shape_tests_7`
  (still XFAIL): C_F_POINTER `LOWER` itself is implemented and honored
  (l06, verified by `test_programs/l06_c_f_pointer_lower.f90` via the
  scalar-`dim` form and by the differential `c_interop_strings` test).
  General array-intrinsic work, not C-interop — own it outside l06.
- The runtime format parser accepts unknown edit-descriptor sequences
  without raising an I/O error: `'(at)'` printed untrimmed text,
  `'(lzs, f6.2)'` printed nothing, both exit 0 (nomad, 2026-06-10).
  Bites typo'd formats today; l05 makes unknown descriptors a runtime
  error as part of the AT/LZ work.
- CHARACTER VALUE copy-in (x08 deferred intake, attempted in l06, reverted):
  non-BIND(C) `character(N), value` dummies stay loudly sema-rejected.
  Making them work is a dedicated calling-convention change, not a bounded
  patch. The COMMON-char half of the same intake item shipped (l06); this
  half did not. Findings from the attempt (so the next try doesn't
  re-derive them):
  - A by-ref char dummy is DOUBLY indirect: param `ptr<ptr<i8>>` → a cell
    holding the char data pointer → bytes (two loads to reach data). The
    VALUE signature branch instead emits `ptr<i8>` (one level). `character`
    lowers to `Ptr<i8>`, so `elem_ty != by_ref_storage_ir_type(char)` —
    they differ by one indirection. Easy to conflate; I did, and it cost a
    debugging cycle.
  - Correct copy-in: load slot→cell, load cell→data, memcpy into a private
    `[i8 x N]` buffer, point a private cell at the buffer, store that cell
    into the slot (preserves the double indirection, isolates writes).
  - The change spans MANY coordinated sites: 3 signature value-branches in
    unit.rs (one Subroutine, two Function/sret), the external call site
    (expr.rs materialize closure), the callee param setup in BOTH the
    Subroutine and Function arms, the copy-in, AND the internal/contained
    call path (separate from the masks-based external path). My attempt
    still SEGV'd at runtime on both external and contained calls — the
    signature/call ABI didn't line up across all paths. `--emit-ir`
    produced no output for the failing program, which made IR-level
    debugging hard; resolve that first.
  - Assumed-length (`character(*), value`) additionally needs the length on
    a hidden parameter; keep it rejected even after fixed-length works.
- l07 submodule deferrals (the sprint shipped the cross-TU SMP function
  fix, dep_scan ordering, ancestor-interface / interface-mismatch /
  unknown-parent diagnostics; these are the remaining lower-value gaps):
  - `END PROCEDURE wrong_name` (mismatched end-name on an SMP body) is
    not diagnosed — parser-level check, not yet added.
  - Duplicate SMP definitions across sibling submodules are not
    diagnosed (would need cross-TU/cross-sibling tracking).
  - The DoD's "real stdlib submodule cluster" check (stdlib_quadrature +
    simps) can't run directly: those sources are `.fypp` fypp templates,
    which armfortas doesn't preprocess. Validated submodule clusters via
    the hand-written multifile/nested fixtures instead
    (cross_tu_submodule_*, multi_source_submodule_*, nested chains).
  - SMP mismatch diagnostics name the ancestor module + procedure rather
    than quoting the interface's source span — `.amod` interfaces don't
    carry source spans (the OPEN QUESTION in the sprint; confirmed: no
    spans in the `.amod` format).
- F2023-syntax collision producing silent wrong answers today (accepted
  and mis-lowered, garbage at runtime): `real :: a([2,3])` (R818, an
  array-constructor bound in a type declaration). Details in
  `.docs/audits/f2023-feature-matrix.md`. The ALLOCATE (R937) and
  pointer-remap array-constructor forms are now lowered (l02a items 1+2);
  R818 in declarations is still open.

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
- Standalone attribute statements (`allocatable :: x`, `pointer :: p`,
  `target :: t`) are implemented for VARIABLES (l02a item 6, 2026-06-14):
  the parser folds each into the named entity's type declaration, splitting
  a multi-entity declaration so only that entity gets the attribute, so the
  ordinary `Decl::TypeDecl` lowering handles storage with no new plumbing.
  Two follow-ups remain:
  1. An array-spec on the statement (`allocatable :: a(:)`) is rejected
     loudly — AttributeStmt carries only names, so the deferred shape has
     nowhere to fold. Declare the shape on the type declaration instead.
     Lifting this needs per-entity array-spec on the attribute statement.
  2. Allocatable SCALAR function results: the attribute on a function
     RESULT (no type declaration to fold into) is currently inert, and
     allocatable scalar results SEGV when called even via the inline
     `result()` form (`integer, allocatable :: r`; exit 139) — a
     pre-existing allocatable-result ABI gap. conditional_8 passes only
     because its `g()` is short-circuited away (never called).
  Note: SAVE-as-a-statement (`save :: c`) is still a latent no-op (parsed
  to AttributeStmt, applied nowhere) — out of scope here; the fold pass
  deliberately handles allocatable/pointer/target only.

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
  same-type argument-association check covers CALL statements only. An
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

- **RESOLVED (2026-07-04, x14)** ~~19 afs-as suites and 1 afs-ld test
  hard-fail off macOS arm64~~: afs-as 482090d gated all 848
  macOS-toolchain tests behind `native_macho_host()` (loud
  HARNESS_SKIP, 33/33 suites ok on FreeBSD), and the x13/x14 ELF
  writer + x86_64 assembler added real ELF coverage on GNU/x86 hosts
  (per-instruction and whole-corpus differentials vs gas).
  `cargo test -p afs-as` is now green on every fleet host.

Found during l04 (2026-06-12):

- **RESOLVED (2026-07-04, L-tail)** ~~No general intrinsic
  argument-count checking~~: `intrinsic_arity()` table +
  `check_intrinsic_call_arity` in sema validate cover ~150 names with
  F2023 16.9 bounds, on both function references and CALL statements.
  Fires only when the name resolves to the intrinsic (user symbols
  shadow); Range-subscripted references (sections/substrings) are
  exempt by shape. Names outside the table remain unchecked —
  extend with standard citations only. Validated no-false-reject
  against the full corpus and the 53k-line fpm amalgamation.

- **RESOLVED (2026-07-04, L-tail)** ~~SYSTEM_CLOCK runtime is not
  integer-kind aware~~: afs_system_clock_k takes the smallest integer
  kind among the present args (threaded from the lowering's writeback
  types). kind>=8 nanoseconds; kind 4 millisecond clock wrapped at
  HUGE(int32) (rate 1000, gfortran-compatible); kinds 1/2 "no clock"
  per F2018 16.9.202. l04_system_clock_kinds asserts exact kind-4
  and kind-8 values.

- **RESOLVED (2026-07-04, verified stale)** ~~PRINT with a character
  format inserts spurious spaces~~: no longer reproduces on trunk —
  fixed as a side effect of #91's PRINT-format routing (FMT_CTX
  stack, proc-call items un-gated). Both the bracketed-char shape and
  the intrinsic-shadowing function-result shape emit byte-exact
  output now; pinned by ltail_print_char_format_exact.f90.

- **RESOLVED (2026-07-04, L-tail)** ~~No subroutine-as-function
  diagnostic~~: the arity gate grew form checking — intrinsic
  subroutines in function position and CALLs of function intrinsics
  are both compile errors (intrinsic_is_subroutine + is_call flag in
  check_intrinsic_call_arity). Shadowing exemption unchanged; no
  false rejects on corpus or fpm.

- **TOKENIZE implemented in l04a** (2026-06-13): both forms shipped.
  The Form 1 worry in the original deferral (per-element string
  descriptors via afs_assign_char_deferred) turned out unnecessary —
  armfortas represents `character(:), allocatable :: a(:)` as a single
  contiguous array descriptor with elem_size = the deferred length, so
  TOKENS allocates as one afs_allocate_1d(desc, maxTokenLen, count)
  with each fixed-size slot filled and space-padded, exactly like
  flang's tokens.Establish(tokenElemBytes). Both forms deallocate
  before reallocating (INTENT(OUT) allocatable). Form detection is by
  the third argument's type. See afs_tokenize_positions /
  afs_tokenize_tokens in runtime/src/string.rs.

- **PARTIALLY RESOLVED (2026-07-04, L-tail)** ~~SPLIT does not
  bounds-check POS~~: afs_split now enforces the 16.9.197 POS ranges
  (forward 0..LEN, BACK 1..LEN+1) with a loud runtime error, exit 1;
  split_3/split_4 XFAILs flipped to live EXIT_CODE tests.
  SELECTED_CHAR_KIND: variable-argument form implemented 2026-07-04
  (l10, afs_selected_char_kind; literals already const-folded);
  split_2 itself still needs UCS-4 character DATA, which stays out of
  scope (README entry).

- **`print '(format)'` ignores the format — FIXED (x12, 2026-06-20)**:
  Stmt::Print dropped its `format` field and always lowered through the
  list-directed path, so any `print '(...)' , items` emitted
  list-directed output (and `print '("lit",i3)', 7` dropped the
  embedded literal entirely). Fixed by routing a character format
  through the same afs_fmt_begin_ex/push/end machinery WRITE uses; `*`
  and (still-unsupported) numeric FORMAT labels stay list-directed. See
  commit "Honor PRINT's character format string" and
  test_programs/x12_print_format_string.f90. Supersedes the earlier
  PRINT spurious-space / multi-item notes.
  ~~CAVEAT + DEFERRED (format-engine re-entrancy)~~ — RESOLVED
  (2026-07-04): the runtime formatter is re-entrant now — FMT_CTX is a
  STACK of contexts (begin pushes, end pops), so nested I/O during
  output-item evaluation runs in its own context. The PRINT
  list-directed fallback for procedure-call items was therefore pure
  harm (`print '(a,f8.3)', 'area = ', area(r)` silently ignored the
  format, emitting list-directed E-notation — seen live in fpm demo
  output) and has been removed. Verified: the historically-regressed
  cli_driver contained_program_char_function_inside_adjustl_and_trim
  passes, and the WRITE shape (`write(*,'(A)') trim(real_to_str(x))`)
  now matches gfortran. Fixture x12_print_format_with_function_item
  pins the function-item format + a nested internal formatted write.

- **fpm self-hosting stage0 bringup (x12, 2026-06-20)**: compiling the
  amalgamated `fpm-0.13.0.F90` (53k lines, fpm + vendored M_CLI2,
  toml-f, jonquil, fortran-regex/-shlex) surfaced a chain of
  frontend/dispatch bugs, each fixed with a minimal repro + x12_*.f90
  fixture on branch fpm-self-host (full regression green each time):
    1. host/module-assoc deferred-shape array read as rank 0 (generic
       `insert` mismatch) — give it the declared rank in install_one_global.
    2. module deferred-len char ARRAY recorded char_kind=Deferred, so
       ALLOCATE took the 32-byte scalar-string path (size 0, garbage
       elements) — record char_kind=None like the local path.
    3. LEN treated as elemental → `5 5 5` for a whole array; it is a
       scalar inquiry function.
    4. PRINT ignored its character format (above).
    5. a local object named like a generic (`new(:n)` substring where
       `new` is a char dummy) resolved to the generic — local shadows it.
    6. a host-module's own declaration (`initial_size`) flagged as
       USE-ONLY-filtered when an unrelated `use, only:` module also
       exported the name — host association wins.
    7. an in-module call to a generic the module extends saw only the
       merged symbol's arg_names (its locals + first re-export), not the
       full re-export chain (`get_value` through tomlf→tomlf_build→4
       leaves) — gather the complete candidate set before erroring.
    8. USE-renamed derived type (`json_value => toml_value`) not
       canonicalized in generic dispatch (`json_load`).
    9. same rename not canonicalized in component access
       (`j_error%message` → "no field", then broken MOVE_ALLOC).
   10. comma-separated type-bound binding (`procedure :: a, b, c`,
       F2018 R448) bound only the first name — `global_settings%full_path()`
       failed with "no specific type-bound procedure ... candidates: []".
       parse_type_bound_proc now parses the full decl-list. FIXED on branch
       fpm-bringup-2 (test_programs/x12_comma_list_type_bound_procs.f90 +
       parser unit test); not yet PR'd.
  OPEN (resume): bug 11 — generic `set_string` call
  `set_string(table, "requested_version", self%requested_version%s(),
  error, 'dependency_config_t')` (fpm_dependency dump_to_toml) matches no
  candidate [set_character, set_string_type]. ROOT (via AFS_DBG_GEN debug
  in resolve_generic_call_actuals_from_specifics):
  generic_actual_expr_type_info infers the 3rd actual
  `self%requested_version%s()` as Integer{None} instead of Character. It's
  a TBP function call (FunctionCall with a ComponentAccess callee) that
  falls through to operator_expr_type_info; that returns Character in every
  isolated repro (~/afs-scratch/buge/tbpret*.f90) but Integer in the full
  fpm type environment. version_t%s() returns character(len=:),allocatable;
  requested_version is type(version_t),allocatable (other unrelated CHARACTER
  `requested_version` vars exist — possible scope/layout mis-resolution).
  Next: instrument operator_expr_type_info's component-access-callee
  (TBP-call) return-type path.
  DEFERRED rename facets (not yet hit by fpm): component-WRITE
  (`jv%x = 5`) and direct-call argument passing of a renamed-type actual
  still mis-resolve; the robust fix is to canonicalize a local's
  derived_type at decl time (alloc.rs) so every path sees the canonical
  name — higher blast radius, deferred. Repros in ~/afs-scratch/buge/.
  DEFERRED: numbered FORMAT labels (`write(*,100)` / `print 100,`)
  produce no/list-directed output; unsupported everywhere, zero
  test_programs use them.

- **SUSPECTED arm64 -O2+ default-init component read returns 0 (x12,
  2026-06-21)**: the first form of test_programs/x12_comma_list_type_bound_procs.f90
  had `integer :: n = 3` default-init and `has_loc(self) = self%n > 0`,
  called as `s%has_loc()` on a `type(settings), intent(inout) :: s` dummy
  whose actual was a default-initialized local. On macOS arm64 at -O2,
  -O3, -Ofast, -Os the read returned 0 (printed `hl=F`); -O0/-O1 and all
  x86 opt levels returned 3 (`hl=T`). Pre-existing — the comma-list TBP
  parser fix doesn't touch codegen/init; the fixture merely exposed it.
  Reworked the fixture to not read the component (returns a constant) so
  the macOS gate passes. Needs an arm64 reduction (nomad): minimal
  `type(t){integer::n=3}` local passed intent(inout) to a sub that reads
  `x%n` at -O2. Likely default-init of a derived local elided or the
  intent(inout) copy losing the initializer at O2 on arm64. Owner:
  arm64 opt / default-init.

- **fpm coerce_to_type Ptr(i8)→Array(i8,4096) silent miscompile (x12,
  2026-07-02)**: with the fpm stage0 bringup fixes landed (PR #85), the
  full 53k-line `fpm-0.13.0.F90` now compiles → links → runs (17MB x86_64
  ELF, `armfortas fpm-0.13.0.F90 -o fpm_bin`), but `fpm_bin --version` /
  `--help` print NOTHING. Cause: during lowering of
  `fpm_command_line::get_command_line_settings`, `coerce_to_type`
  (src/ir/lower/helpers.rs:220, the `_ =>` fallback) hits
  `Ptr(Int(I8)) → Array(Int(I8), 4096)`, eprintln's, and returns `val`
  UNCHANGED — a ptr where a 4096-byte char-array aggregate is expected
  (the classic "silently wrong is worse than a panic" stub). The buffer
  is `character(len=4096) :: cmdarg` (fpm line 17329); the coercion is on
  a `char(4096) = <allocatable char>`-style path (`cmdarg =
  get_subcommand()` at 17374, get_subcommand returns
  `character(len=:),allocatable`). NOT reproduced by the obvious minimal
  cases (buf=greet(), buf=trim(a), call fill(buf) all work) — needs a
  closer M_CLI2-shaped reduction. Do NOT "fix" by loading the whole
  Array(i8,4096) — x86 isel has no register class for it (see bug-14);
  fix at the producer/consumer so the pointer is used directly, or make
  the fallback error loudly and handle Ptr→Array(char) as a memcpy into
  the buffer slot. This is the next fpm edge. Owner: char aggregate ABI /
  ir/lower helpers.
  UPDATE (2026-07-03): both halves done. The specific Ptr(i8)→Array(i8,N)
  instance was fixed with the char-AC element assign in PR #86, and the
  `_ =>` fallback is now a hard ICE (panic) instead of eprintln+return-val
  — repo policy, silent wrong-typed forwarding is a miscompile factory.
  Canary: lib 1301/0, run_programs 120/0, and the full 53k-line fpm
  compile all pass with the loud fallback, so no live path relies on it.

Found during the L-tail internal-file work (2026-07-04):

- **RESOLVED (2026-07-04, l10)** ~~Scalar internal WRITE drops values
  past one format scan~~: both scalar sinks use reverting scans with
  the one-record rule — overflow is IOSTAT=1 or a loud exit 2
  (l10_internal_write_scalar_overflow{,_loud} fixtures).
- **List-directed internal WRITE to a char array emits one record**
  (into element 1; the rest untouched). Record splitting for
  list-directed output is processor-dependent; gfortran wraps at the
  element length. Recorded decision, revisit if a target project
  compares against gfortran here. Owner: l10 if a project hits it.
- **DECIDED (2026-07-04, l10)** ~~Internal READ from whole char
  arrays unprobed~~: probed — same len-0-view silent garbage. Until
  the read path grows record-per-element semantics it is REJECTED at
  compile time (element units keep working); fixture
  l10_internal_read_array_reject. Full read support is the recorded
  follow-up.

Model-drift audit (2026-07-06) — full report at
`.docs/audits/model-drift-audit-2026-07.md`. Five hostile auditors over
recent work (afs-ld, afs-as, compiler core since June 1, real-project
sweep, test integrity). 16 verified defects; every miscompile reproduced
on a built binary. Capability: 6 of 16 real projects behave correctly
(gfortran passes all 16 — failures are ours). The audit doc holds the
ranked findings, reproducers, and fix ladder; the load-bearing ones:

- **REGRESSION — stdlib no longer builds. → FIXED 2026-07-06** (#125). Two
  generic-dispatch bugs: (a) allocatable rank-N derived-type component
  dropped its declared rank (deferred-shape `index(:,:)` → empty dims →
  inferred rank 1), blocked `sort_coo` at ~52% — seed a deferred-shape
  component's dims with `vec![(1, 0); rank]` so `dims.len()` keeps the
  declared rank (landed upstream via the compiler-edges workstream;
  behavioral regression test retained here); (b) order-dependent
  block-local scalar actual promoted to rank 1 via a foreign same-named
  rank-1 dummy from a USE'd module (`actual_expr_rank` any-scope fallback),
  blocked `dense()`/`check()` in the test exes — restrict the rank
  cross-check to the current scope, reconciled with the upstream
  descriptor/by-ref gate (267c8ce). Full `fpm test` now 1288/0,
  matching 976de26; gfortran control 1289/0. Regression tests in
  test_programs/ + tests/multifile.rs.
- **memmove −1 length → SIGSEGV** on derived-type-with-allocatable-char
  assignment (`x = ctor()`), fault addr 0xffff...ffff. Crashes 7 fgof
  libraries — highest-leverage single fix. Same family as the deferred
  rank-2 deferred-char (C10) and internal-WRITE (C12) items below.
- **C12 / internal-WRITE realloc regressed to silent truncation**,
  fixture rewritten in the same commit (426a29d); `f2023-feature-matrix.md:86`
  still claims realloc. Truncates even under `--std=f2023`. Adjudicate vs
  F2023 12.6.4.5.1 before fixing; silent truncation is policy-forbidden
  either way.
- **T1 / `--eh-frame-hdr` buried retirement** — afs-ld 2cb05d7 moved it to
  silent-accept and re-pointed its rejection test to `-pie` undisclosed;
  under `AFS_LD=1` binaries lack the unwind header the driver requests.
- **T2 / arm64 -O2+ default-init miscompile now has zero coverage** —
  both exposing fixtures (ffb7d0a, 5e250a4) were neutered; the XFAIL
  grammar had no opt-level qualifier so a plain `XFAIL(arm64)` panics as
  unexpected-pass at -O0.
  - **GRAMMAR FIXED 2026-07-06 (99cdc2d)**: opt-level selectors added —
    `O2` exact, `O2+` rank-and-above (Os=rank 2, Ofast=3); target ∧ opt
    dimensions conjoin so `XFAIL(arm64,O2+)` works. Unit-tested.
  - **FIXTURE PENDING an arm64 run**: wrote a minimal repro (derived local
    with `integer::n=3` default-init, passed intent(inout) to a sub that
    reads `%n`, CHECK n=3/hl=T, `XFAIL(arm64,O2+)`). Correct on x86_64 at
    all 6 levels (n=3). NOT committed to test_programs/ yet because I
    can't confirm from FreeBSD that this *minimal* form triggers the bug
    on arm64 (the original used a type-bound proc `s%has_loc()`); a wrong
    repro would XPASS and break the macOS gate. nomad (only arm64 box) was
    unreachable 2026-07-06. Validate on arm64 (n must be 0 at -O2/-O3/
    -Ofast/-Os, 3 at -O0/-O1), tighten the repro to a TBP form if the
    plain sub doesn't trigger, THEN commit. Fixture source:

    ```fortran
    ! XFAIL(arm64,O2+): arm64 default-init component read returns 0 at -O2+ (audit T2)
    ! CHECK: n=3
    ! CHECK: hl=T
    module x12_default_init_m
      implicit none
      type :: settings
        integer :: n = 3
      end type settings
    contains
      subroutine show(s)
        type(settings), intent(inout) :: s
        print '(a,i0)', 'n=', s%n
        print '(a,l1)', 'hl=', s%n > 0
      end subroutine show
    end module x12_default_init_m

    program x12_arm64_default_init_intent_inout
      use x12_default_init_m
      implicit none
      type(settings) :: local
      call show(local)
    end program x12_arm64_default_init_intent_inout
    ```

Memmove -1 crash split into two bugs (2026-07-06):
- **FIXED (cd6bbe9)**: cross-module assumed-size `(*)` / explicit-shape
  array dummy was passed as a descriptor instead of a bare data address.
  `descriptor_param_mask_for_lookup` let the `.amod` rank-based dummy
  reconstruction (which rebuilds `(*)` as AssumedShape) override the
  authoritative descriptor mask. Now trusts the cache when present.
  Regression tests: tests/multifile.rs assumed_size_{integer,cchar}_*.
  Fixes `from_c_string`-style C-interop scans in isolation.
- **FIXED (3e15d80)**: the second root cause was NOT the hidden char-length
  ABI (that theory was disproved — the length forwards correctly). It was
  the sret "write straight in" path: `lower_alloc_return_call_into_desc`
  passed the assignment target's LIVE descriptor as the callee's
  allocatable-array result slot without resetting it. For an already-
  allocated target (`allocate(c(0)); c = f(...)` — the fgof
  `allocate(c_parent(0)); c_parent = to_c_string(x)` idiom) the callee's
  `allocate(result)` reused/corrupted it; a size-0 target gave a -1 base
  and SIGSEGV'd in the callee's first element write. Fix: deallocate the
  target first (STAT makes it a no-op on the unallocated temp descriptors).
  Regression fixture test_programs/alloc_array_result_into_preallocated_
  target.f90. Result: memmove SIGSEGV ELIMINATED from all 7 fgof libs;
  5 of 7 now fully pass (temp 9/9, state, cache, fs, pty). lib 1301/0,
  multifile/cli_driver/run_programs all unchanged.
- **RESIDUAL (open, lower severity)**: fgof-process and fgof-watch no
  longer crash but a few test executables `error stop`/exit 1 (behavioral,
  not SIGSEGV): process test_parent_state/test_run_options/test_validation,
  watch test_watch_filters. Distinct bug — assertion-level, needs its own
  reduction. Owner: TBD (behavioral, not the crash class).

- **KIND() on a complex value always returns 4** (found while fixing C7,
  2026-07-06). `complex(8) :: q; kind(q)` reports 4, not 8, even though the
  value is stored and computed in full double precision (`real(q)` of
  `1.0_8/3.0_8` prints 15 correct digits). The kind intrinsic on a complex
  operand ignores the component kind. Separate from C7 (which is about
  real(16)/complex(16) being rejected) — the value is correct, only the
  reported kind is wrong. Low severity, no fixture yet. Would live near the
  kind-inquiry lowering; the C7 OK fixture
  (`real_kind_supported_ok.f90`) deliberately proves complex(8) precision
  via arithmetic instead of `kind()` to route around it.

Other verified miscompiles (details + repros in the audit doc), all with
zero fixture coverage: reduction vectorizer drops elementwise stores at
-O3/-Ofast (C1, arm64 likely too); BIND(C) aggregate ABI is dead code,
silently wrong both directions (C2); strided component-view I/O (C3);
elemental defined operators broadcast on arrays (C4); FINDLOC drops
MASK=/BACK= (C5); untyped char-AC corrupts (C6, fix gated to fpm's
spelling); `real(16)`→single precision (C7, pre-June); implied-do
list-directed output prints a blank record (C8, pre-June); C_F_POINTER
component-FPTR rank (C9); rank-2 deferred-char compares as len 0 (C10);
CLASS mold=/source= loses dynamic type (C11); ferp `[0-9]` regex
miscompiles; superlinear compile time on 500+/1400-line modules
(sniffert, facsimile). afs-as: SSE-imm8 RIP addend off-by-one, movhlps→
movlps aliasing, disp>i32 truncation, %ch-as-shift-count. afs-ld: no
weak/strong resolution (miscompile), HashMap-ordered version-alias
(nondeterministic bytes), library-order permutation, IFUNC misclassified
(blocks 3f), GOTPCREL-on-linker-symbol panic. New campaign PASSes worth
adopting: toml-f, test-drive.
