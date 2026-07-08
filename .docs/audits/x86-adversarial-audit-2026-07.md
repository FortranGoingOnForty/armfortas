# x86_64 adversarial audit — 2026-07-07

Eighteen hostile finders ran in parallel against the x86_64-linux-gnu target:
six synthetic dimensions (ABI, optimizer, language edges, stress/hot-paths,
nondeterminism, toolchain afs-as/afs-ld) and twelve real projects from the
campaign ladder. Every finding was independently re-reproduced by a skeptical
verifier that tried to refute it (conformance under `gfortran -std=f2018
-Wall -Wextra -fcheck=all`, reference agreement at two opt levels, full
-O0…-Ofast sweep, environment/determinism checks, duplicate search against
`noted_items.md` and `.docs/audits/`). Host: kasumi (CachyOS x86_64),
gfortran 16.1.1 reference, armfortas release binary from branch
`ar-remediation` (built 2026-07-07 15:35; the branch moved during the audit —
key findings were re-verified against rebuilt binaries up through `5df42ab0`,
and every finding in the self-assignment family was confirmed live
post-`606b54f8`).

All repros need `export AFS_CRT_DIR=/usr/lib/gcc/x86_64-pc-linux-gnu/16`
(the `run.sh` scripts in each repro dir set it themselves). Repro dirs live
in the repo root, untracked: `verify-*` (verifier-reduced repros) and
`verify-audit-scratch/` (sources and scripts salvaged from the audit
session's scratchpad; binaries were dropped — each dir's `run.sh` rebuilds
them). Delete both once their findings are fixed and fixtured.

## Verdict

The finders filed 66 reports; verification refuted 3, mapped 2 onto
already-tracked open items, and confirmed 61, which dedup by root cause to
**54 distinct findings: 18 critical, 14 high, 9 medium, 13 low**. Two are
regressions: C13 was introduced by the `3e15d80` memmove-family fix itself,
and D1 is a driver capability the fix ledger records as landed-with-test that
never merged past a campaign branch.

The rot concentrates in five seams. (1) **Character-array lowering** — the
`char_kind != None` branches of `lower_array_assign` and the array-constructor
paths hold seven independent holes (C18, C19, C23, C24, C25, C33, C34); the
char destination path broadcasts, skips overlap snapshots, compares pointers
instead of bytes, and drops type-spec lengths. (2) **Aliasing/self-assignment
snapshots** — each fix in this family (`606b54f8`, `3e15d80`) covered exactly
its reproducer's path and left the sibling paths open (C13, C18, C20).
(3) **Derived-type I/O dispatch** — `lower_write_items_adv` and
`fmt_push_whole_array` have no whole-derived case, so values vanish or the
compiler ICEs (C21, C22, C31). (4) **Legacy storage association** — COMMON
array members, whole-array DATA, and character EQUIVALENCE are effectively
unimplemented and fail silently (C16, C17, C24). (5) **The runtime I/O layer**
— thirteen R findings, including a structural silent-swallow: `afs_fmt_end`
detects format errors internally but PRINT/WRITE without IOSTAT= discards them
and drops the record (R1, and the same shape in C21/C22) — precisely the
silent-degrade pattern the repo's own policy forbids.

Against the 2026-07-06 model-drift audit: that audit closed its compiler-core
rung (C1–C12) and none of those fixtures regressed here — but its diagnosis
("a `_ => {}` dispatch gap on a type/rank/keyword, and fixes shaped like their
reproducer") describes 40+ of these 54 findings. The disease wasn't cured, the
tested paths were. The toolchain, by contrast, held up: afs-as and afs-ld
yielded only medium/low findings after the A1–A6/L1–L9 waves, and the
gas-differential over ~560 compiled programs × 4 opt levels found zero real
byte divergences. The cross-level correctness invariant also mostly holds:
48 of 54 findings reproduce identically at every opt level (front-end,
lowering, or runtime bugs); the optimizer/backend proper contributes three
wrong-code/crash findings (C29 backend regalloc, C30 dead-arg, C36 unroll
ICE), one compile-time blowup (C38), and two byte-determinism breaks
(C37, C42).

One positive drift: the ferp `[0-9]` regex miscompile (open FAIL in the
model-drift audit) no longer reproduces, and the fgof-process residual
error-stops in `noted_items.md` now pass — both entries should be flipped
with fixtures (see Known duplicates).

## Severity-ranked findings

House rule throughout: **the fixture lands with the fix**; suggested names use
the `ar2_` prefix.

### Regressions

- **C13 — `x = f(x)` with an allocatable-array function result deallocates
  the target before the call; the callee reads a freed descriptor and
  SIGSEGVs. CRITICAL, REGRESSION (3e15d80).** crash/use-after-free; all six
  opt levels. Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-proj-fgof-watch-ir-selfassign-allocres-uaf-1`
  (`$AFS -O0 reduced.f90 -o r && ./r` → exit 139; gfortran prints `11 21 31`,
  exit 0). The `3e15d80` fix for the memmove−1 family made
  `lower_alloc_return_call_into_desc` deallocate an already-allocated
  assignment target before reusing its descriptor as the sret slot — but it
  fires even when the target is also an actual argument of the RHS call.
  `--emit-ir` shows `afs_deallocate_array(%0)` immediately before
  `call @afs_modproc_m5_pass_through(%0, %0)`; gdb faults in the callee with
  rdi=0. Scalar deferred-char `s = f(s)` takes a different path and is fine.
  Kills fgof-watch `test_watch_debounce`; the noted_items claim that
  fgof-watch "no longer crash[es]" post-3e15d80 is stale. Fix: defer/skip the
  dealloc when the destination name appears in the call's actuals (snapshot
  descriptor into a temp, dealloc after the call). Fixture:
  `test_programs/ar2_selfassign_alloc_result.f90`.

  ```fortran
  module m5
  contains
    function pass_through(raw) result(events)
      integer, intent(in) :: raw(:)
      integer, allocatable :: events(:)
      events = raw + 1
    end function
  end module
  program red5
    use m5
    integer, allocatable :: x(:)
    x = [10, 20, 30]
    x = pass_through(x)   ! SIGSEGV: x freed before the call
    print *, x
  end program
  ```

- **D1 — mixed source+object invocation rejected again: commit `82c947e5`
  is recorded FIXED in x86-campaign-log.md with a named regression test, but
  was never merged; the test does not exist on trunk. HIGH, REGRESSION.**
  driver; opt-independent. Two finders hit it independently (nondet, fgof-fs).
  Repro: `verify-audit-scratch/verify-nondet-driver-mixed-input-lost-fix-1`
  (`bash run.sh`): `armfortas main.f90 lib.o -o app` → exit 1, "mixing
  Fortran sources with prebuilt object/archive inputs is not yet supported" —
  the `(true,true)` reject arm at `src/driver/mod.rs:1258`. gfortran accepts
  the identical invocation. `git merge-base --is-ancestor 82c947e5 trunk`
  fails; the commit is reachable only from `origin/x12-campaign-x86`;
  `tests/multifile.rs` has no `mixed_source_and_object_in_one_invocation`.
  Yet `.docs/audits/x86-campaign-log.md:415-419` records it FIXED with that
  test. Fix: merge/cherry-pick `82c947e5` (compile_multi partitions sources
  from prebuilt artifacts, links in command order) plus its test. **Process
  finding (T5):** a ledger entry can record FIXED against a commit that never
  reached trunk. Audit every FIXED entry in the campaign logs for commit
  ancestry, and adopt the rule that FIXED requires the commit to be an
  ancestor of trunk.

### Compiler core — critical

- **C14 — strided array section passed to an explicit-shape/assumed-size
  dummy gets no copy-in/copy-out; the raw descriptor base is passed.
  CRITICAL.** wrong-code + OOB writes; all levels; single- and cross-TU;
  internal, module, and external callees. Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-abi-abi-secpack-1`
  (`bash run.sh`, `run_reduced.sh`). `call bump(v(1:7:2), 4)` with dummy
  `a(n)` updates v(1..4) contiguously (`1001 1002 1003 1004 5 …`) instead of
  v(1),v(3),v(5),v(7); `call bump(v(10:1:-1), 4)` strides forward off the
  array end — 3 OOB stores, then SIGSEGV. gfortran creates the mandated array
  temporary (its runtime says so under -fcheck=all). Root:
  `src/ir/lower/core.rs::lower_arg_by_ref_full` (~54010): the Ptr<[i8;384]>
  fallthrough extracts the descriptor's base_addr and passes it bare for any
  rank≥1 actual, no contiguity check, no pack temp — the x86-campaign Bug #3
  fix commented this path as "correct for an array section", which is exactly
  the gap. `intent(in)`-only takes a different (correct) path. Fixture:
  in-tree `test_programs/ar1_secpack.f90` already encodes the expectations —
  flip it to real CHECKs with the fix.

  ```fortran
  subroutine bump(a, n)
    integer, intent(in) :: n
    integer, intent(inout) :: a(n)
    integer :: i
    do i = 1, n
      a(i) = a(i) + 1000
    end do
  end subroutine
  program p
    integer :: v(10), i
    v = [(i, i=1,10)]
    call bump(v(1:7:2), 4)
    print '(10(i0,1x))', v   ! afs: 1001 1002 1003 1004 5... ; gf: 1001 2 1003 4 1005 6 1007 8 9 10
  end program
  ```

- **C15 — pointer assignment to a strided section drops the stride:
  `q => v(3:9:3)` aliases v(3:9) with extent 7. CRITICAL.** wrong-code (reads
  AND writes through the pointer); all levels. Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-abi-ptr-stride-drop-1`
  (`bash run.sh` / `run_reduced.sh`). Expected lb/ub/sum `1 3 18`, q=[3,6,9],
  `q(2)=100` writes v(6); actual `1 7 42`, q aliases the contiguous range,
  `q(2)=100` writes v(4). Passing the same section as an assumed-shape actual
  is correct — the defect is specific to the `=>` lowering: the emitted
  descriptor stores a literal stride of 1 and extent (ub−lb+1) instead of the
  triplet's stride operand (`src/ir/lower/stmt.rs` PointerAssignment,
  FunctionCall fallthrough below the ~8850 section-RHS path). Fixture:
  `test_programs/ar2_ptr_section_stride.f90`.

  ```fortran
  program p
    integer, target :: v(9)
    integer, pointer :: q(:)
    v = [1,2,3,4,5,6,7,8,9]
    q => v(3:9:3)
    print *, ubound(q,1), sum(q)   ! afs: 7 42 ; gf: 3 18
  end program
  ```

- **C16 — COMMON block array members are unusable: whole-array reference
  reads one garbage value, element assignments vanish from the IR, element
  access and `sum()` lower to undefined externals (link failure). CRITICAL.**
  wrong-code + build-failure; all levels; single- and cross-TU. Repro:
  `verify-audit-scratch/verify-abi-common-array-member-1` (`bash run.sh`;
  `armfortas reduced.f90 -o r && ./r` prints ` 0` vs gfortran `7 8 9`; the
  `ia(1)`/`sum(ia)` variant fails to link: `undefined reference to 'ia'`,
  `'sum'`). Scalar members in the same block are fine. Root:
  `src/ir/lower/core.rs::install_common_locals` (~2823) sets `dims: vec![]`
  for every member and sizes storage by the scalar element type — array
  members become 4-byte scalar globals, stores to elements are dropped, and
  indexed references fall through to unknown-identifier → external call. No
  test_programs file declares an array in a COMMON block. Fixture:
  `test_programs/ar2_common_array_member.f90`.

  ```fortran
  program reduced
    integer :: ia(3)
    common /blk/ ia
    ia(1) = 7; ia(2) = 8; ia(3) = 9
    print *, ia          ! afs: " 0" ; gf: "7 8 9"
  end program
  ```

- **C17 — whole-array DATA with a plain value list leaves the array
  uninitialized. CRITICAL.** wrong-code (per-run stack garbage); all levels.
  Repro: `verify-audit-scratch/verify-stress-stress-data-plain-list-uninit-1`
  (`armfortas -O0 reduced_final.f90 …`). `integer :: a(5); data a /17,34,51,
  68,85/` prints varying garbage; `data x /99/` in the same unit works.
  Root: `src/ir/lower/init.rs:530` — the `Decl::DataStmt` arm does
  `if !info.dims.is_empty() { continue; }`, skipping every array target
  unconditionally (its own comment discloses only implied-do and `r*v` as
  gaps). `--emit-ir` confirms zero stores are generated. Found via a
  2000-entry lookup table whose checksum came out 127890146820 instead of
  249719. The nondet finder also saw DATA-init of COMMON members dropped —
  cover both in the fixture. Fixture: `test_programs/ar2_data_whole_array.f90`.

- **C18 — overlapping section assignment copies forward with no RHS snapshot
  for derived-type and character arrays. CRITICAL (merged: opt
  derived-overlap-shift-1 + char-overlap-shift-1).** wrong-code; all levels.
  Repros: `verify-audit-scratch/verify-opt-derived-overlap-shift-1` and
  `verify-audit-scratch/verify-opt-char-overlap-shift-1` (`bash run.sh` each).
  `p(2:5) = p(1:4)` on `type(t) :: p(5)` yields `10 10 10 10 10` instead of
  `10 10 20 30 40`; `c(2:5) = c(1:4)` on `character(2) :: c(5)` yields
  `A1 A1 A1 A1 A1`. Integer/real arrays are handled. One root:
  `src/ir/lower/core.rs::lower_1d_section_assign` (~44730-44775) gates its
  overlap-snapshot block on `dest_info.derived_type.is_none() &&
  dest_info.char_kind == CharKind::None && !descriptor_backed_runtime_char_
  array(...)`, then streams an ascending element copy
  (`lower_derived_array_copy_loop` for derived, per-element char copy for
  character) straight from the unsnapshotted source. Same family as C13/C20;
  `606b54f8` fixed only the whole-array vector-subscript path. Fixture:
  `test_programs/ar2_overlap_section_derived_char.f90` (both element types,
  forward and reversed shifts).

- **C19 — whole-array assignment to a character array with an array-valued
  RHS takes the scalar-broadcast path: sections write descriptor pointer
  bytes into every element; function results duplicate element 1. CRITICAL
  (merged: opt char-rev-ptrbytes-1 + lang charfunc-array-dup-1).** wrong-code
  + address leak + SIGABRT cascade; all levels. Repros:
  `verify-audit-scratch/verify-opt-char-rev-ptrbytes-1` and
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-lang-lang-charfunc-array-dup-1`.
  `c = c(3:1:-1)` on `character(3) :: c(3)` fills every element with the same
  3 ASLR-varying bytes (the on-stack section descriptor's base-address
  field); when those bytes aren't valid UTF-8 a later PRINT panics at
  `runtime/src/format.rs:990` → SIGABRT. `r = names()` (a `character(3)`
  array function) prints `abc abc` instead of `abc def`. One root: in
  `lower_array_assign` (`src/ir/lower/core.rs` ~46404), when
  `dest_info.char_kind != CharKind::None` the code calls
  `lower_string_expr_ctx` once and runs the `char_broadcast` loop with the
  same (ptr,len) for every element — correct for `c = 'xyz'`, wrong for any
  array-valued RHS (section, function call). Direct constructor assignment
  `r = ['abc','def']` works, so only the misclassified-RHS entry is broken.
  Fixture: `test_programs/ar2_char_array_rhs_assign.f90` (section-RHS,
  reversal, function-result RHS).

- **C20 — whole-object self-assignment of a derived scalar with an
  allocatable array of derived entries deallocates nested allocatable-char
  components. CRITICAL.** wrong-code (silent data loss, exit 0); all levels;
  confirmed live at HEAD `5df42ab0` from a fresh worktree build. Repro:
  `verify-audit-scratch/verify-proj-fgof-temp-selfassign-nested-alloc-1` (`sh run.sh`).
  After `g = g` where `g%entries(:)` holds `entry_t` with
  `character(:), allocatable :: path`, every `path` is unallocated. Variant
  probes: scalar type with a direct allocatable char is fine; bare
  `arr = arr` is fine post-`606b54f8` — only the array-nested-inside-scalar
  shape is open. Root: the scalar derived-type assignment lowering
  (`src/ir/lower/stmt.rs`, the field-copy paths near the
  `derived_layout_needs_deep_copy` calls ~6238/6674) deep-copies
  component-by-component with per-component dealloc/realloc and has no
  self-alias snapshot. Real-world effect: fgof-temp's `temp_guard` leaks
  on-disk files. Fixture:
  `test_programs/ar2_derived_selfassign_nested_alloc.f90`.

  ```fortran
  program reduced2
    type :: entry_t
      character(len=:), allocatable :: path
    end type
    type :: guard_t
      type(entry_t), allocatable :: entries(:)
    end type
    type(guard_t) :: g
    allocate(g%entries(1))
    g%entries(1)%path = 'alpha'
    g = g
    print '(l1)', allocated(g%entries(1)%path)   ! afs: F ; gf: T
  end program
  ```

- **C21 — the output-item lowering has no whole-derived-scalar case:
  list-directed PRINT of a derived scalar emits a blank record; unformatted
  WRITE emits a zero-length record and the read-back silently fills the
  variable with uninitialized stack. CRITICAL (merged: lang
  derived-listout-blank-1 + nondet io-unf-derived-record-drop-1).**
  io-divergence/wrong-code; all levels. Repros:
  `/var/tmp/verify-lang-derived-listout-blank-1` and
  `verify-audit-scratch/verify-nondet-io-unf-derived-record-drop-1` (`run_reduced.sh`
  each; the unformatted case: `xxd r.bin` shows `00000000 00000000` — length
  markers, no payload; printed read-back values vary under ASLR, stable under
  `setarch -R`). One root: `lower_write_items_adv` (`src/ir/lower/core.rs`
  ~29700-30050) has cases for arrays/complex/char/component-sections but a
  bare derived-type Name falls to the generic branch, which treats the Ptr as
  a string and calls `afs_write_string(unit, ptr, string_literal_len(item))`
  = length 0. Adjacent to the fixed C8 (same function, different missing
  case). Secondary runtime gap to fix together: the unformatted READ does not
  enforce record length — reading 2 ints from a 1-int record reports
  iostat=−1 (EOF) instead of a positive error (gfortran: 5016). Fixture:
  `test_programs/ar2_derived_io_output_list.f90` (list-directed, formatted,
  unformatted round-trip, short-record iostat).

  ```fortran
  program t
    type :: pair
      integer :: i = 3
      integer :: j = 4
    end type
    type(pair) :: x
    print *, x       ! afs: blank line ; gf: "3 4"
  end program
  ```

- **C22 — direct-access I/O accepted but silently implemented as sequential
  appends: `rec=`/`recl=` dropped by the front end; unformatted direct READ
  routed through the text parser. CRITICAL.** io-divergence (silent wrong
  file content, exit 0 for pure writers); all levels. Repro:
  `verify-audit-scratch/verify-nondet-io-direct-access-silent-1` (`./run_reduced.sh`,
  `write_only.f90`). OPEN(access='direct', recl=16) + WRITE(rec=1)/(rec=3)
  produce an 8-byte packed file instead of gfortran's 48-byte record-placed
  file; the read back dies with "READ: cannot parse integer from '   '".
  Root: `src/ast/stmt.rs` has no `rec` field; the Write/Read lowering in
  `src/ir/lower/stmt.rs` never handles `rec=` — dropped before codegen. The
  runtime already contains complete, dead direct-access primitives
  (`afs_write_direct`/`afs_read_direct`, `runtime/src/io_system.rs:2001-2110`)
  that nothing calls. Fix: wire them through, or loud-reject
  `access='direct'` at OPEN per repo policy. Fixture: promote
  `test_programs/future/io_direct_access.f90` out of `future/`.

- **C23 — character MAX/MIN compares operand ADDRESSES, not string contents:
  3-arg MAX returns the minimum; MIN(variable, literal) SIGSEGVs. CRITICAL.**
  wrong-code + crash; all levels. Repro:
  `verify-audit-scratch/verify-lang-char-maxmin-1` (`bash run_reduced.sh`). Expected
  `cd / cd / ab`; actual `cd / aa / SIGSEGV`. `--emit-ir` shows character
  operands falling into the numeric max/min lowering
  (`src/ir/lower/intrinsic.rs`): `icmp ge <ptr>, <ptr>` on the string
  constants' addresses — 2-arg literal cases only "work" by rodata layout
  luck. A variable operand is loaded as a single i8 byte and the i8 select
  result is passed to `afs_fmt_push_string` as a pointer → wild deref.
  Fix: a character-aware lexicographic compare-and-select (or runtime
  helper). Fixture: `test_programs/ar2_char_maxmin.f90`.

  ```fortran
  program red
    character(2) :: a
    a = 'ab'
    print '(a)', max('ab', 'cd', 'aa')   ! afs: aa (wrong)
    print '(a)', min(a, 'zz')            ! afs: SIGSEGV
  end program
  ```

- **C24 — character-character EQUIVALENCE has no storage association: reads
  come back blank; the reduced scalar form segfaults at -O2+. CRITICAL.**
  wrong-code; all levels. Repro: `verify-audit-scratch/verify-lang-equiv-char-blank-1`
  (`bash run.sh` / `run_reduced.sh`). Numeric EQUIVALENCE works
  (`audit6_b3_equivalence.f90` passes). Root:
  `src/ir/lower/core.rs::install_equivalence_locals` (~2864-3020) hardcodes
  `char_kind: CharKind::None` for every member (~3010), and the group
  geometry sizes character members as 8-byte pointer slots
  (`arg_type_from_decls` → Ptr<i8>, `ir_scalar_byte_size` `_ => 8`
  catch-all) instead of by declared length — every downstream char load/
  store keyed on CharKind takes the wrong path. Fixture:
  `test_programs/ar2_equivalence_char.f90`.

  ```fortran
  program t
    character(4) :: a, b
    equivalence (a, b)
    a = 'WXYZ'
    print '(a)', b     ! afs: blank (or SIGSEGV at -O2+) ; gf: WXYZ
  end program
  ```

- **C25 — typed character array constructor ignores the type-spec length:
  elements are packed at max literal width while `len()` reports the
  type-spec value — payload/descriptor disagreement corrupts data; the
  component-assignment form silently shrinks the length. CRITICAL (merged:
  fgof-process proc-char-ac-typespec-arg-1 + -component-1).** wrong-code;
  all levels. Repros:
  `verify-audit-scratch/verify-proj-fgof-process-proc-char-ac-typespec-arg-1`
  (`./run_reduced.sh`: `call probe([character(len=8) :: "AB", "CDE"])` with a
  `len=*` dummy reports len=8 but element 1 byte 4 reads `C` — the
  neighboring element's data) and
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-proj-fgof-process-proc-char-ac-typespec-component-1`
  (`x%a = [character(len=8) :: "HO","USE"]` on a deferred-len allocatable
  component allocates len=3, dropping the mandated blank padding; the bare
  non-component form is correct). One root family: the char-AC temp
  materialization takes element size from the literals, not the AC type-spec.
  Real-world: fgof-process argv/env-name truncation feeding external
  processes. Distinct from the fixed C6 (crash/corruption on the element
  store path) and from C33 below (no type-spec at all). Fixture:
  `test_programs/ar2_char_ac_typespec_len.f90` (argument + component +
  bare-array contexts).

- **C26 — defined assignment(=) leaks through `USE …, ONLY:` when the module
  is loaded from a `.amod`: intrinsic deep-copy silently replaced by the
  module's assign procedure. CRITICAL.** wrong-code (allocatable components
  silently dropped); all levels; multi-TU only (single-file compile is
  correct). Repro:
  `verify-audit-scratch/verify-proj-fortbite-amod-defassign-only-leak-1/reduced`
  (`bash run.sh`; 3-file build with `-J`, marker print inside the assign
  procedure proves it fires in a scope whose ONLY list excludes
  `assignment(=)`). Suspected mechanism (corroborated, not fixed):
  `named_interface_specific_candidates` (`src/ir/lower/core.rs` ~12749) has
  an unconditional `st.lookup("assignment(=)")` fallback that bypasses
  ONLY-filtering for interfaces loaded globally from `.amod`. Per F2018
  10.2.1.4 the assignment must be intrinsic here. Fix-side test matrix should
  also cover defined OPERATORS and defined I/O leaking the same way (only
  assignment was probed). Fixture: cross-TU case in `tests/multifile.rs`.

- **C27 — the preprocessor strips C `/* */` block comments unconditionally
  from plain .f90: a `/*` inside a Fortran `!` comment silently deletes the
  following source lines. CRITICAL.** wrong-code + bogus diagnostics; all
  levels (front end). Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-proj-facsimile-proj-facsimile-cblock-comment-1`
  (`./run.sh`; reduced prints 1, gfortran 42; `--emit-tokens` shows the
  swallowed line produces zero tokens). With no closing `*/` the rest of the
  file is eaten and a bogus "parse error: expected end module" surfaces —
  currently blocks facsimile's `syntax_highlighter_module.f90`
  (gfortran-clean) at line 970 for a `/*` at line 1055. Root:
  `src/preprocess/mod.rs:1435 strip_c_block_comments_from_line`, called
  unconditionally at :333 for every file (no -cpp gate); it is quote-aware
  but not `!`-comment-aware. Fixture:
  `test_programs/ar2_bang_comment_cstyle.f90`.

  ```fortran
  program r
    integer :: x
    x = 1        ! /*
    x = x + 41
    ! */
    print '(i0)', x   ! afs: 1 ; gf: 42
  end program
  ```

- **C28 — ASSOCIATE with a derived-type array-component selector: reads
  print garbage (exit 0), writes SIGSEGV or hang. CRITICAL.** crash + silent
  wrong-code; all levels (segv-vs-hang assignment varies run to run — live
  memory corruption). Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-lang-lang-assoc-comp-array-1`
  (`bash run.sh`; read-only variant t32c.f90 silently prints `*********`).
  Scalar-component associate and plain array-section associate both work —
  the defect is isolated to `associate (w => x%v)` where v is an array
  component (ir-lower selector path). Fixture:
  `test_programs/ar2_associate_component_array.f90`.

  ```fortran
  program t32b
    type :: t
      integer :: v(3) = [1,2,3]
    end type
    type(t) :: x
    associate (w => x%v)
      w(2) = 99
    end associate
    print '(3i3)', x%v   ! gf: 1 99 3 ; afs: SIGSEGV or hang
  end program
  ```

- **C29 — x86 backend emits a store through an undefined register in
  unroll-remainder blocks at -O2/-O3/-Ofast (SIGSEGV; post-opt IR is
  correct). CRITICAL.** wrong-code/crash; -O2,-O3,-Ofast (clean at
  -O0/-O1/-Os). Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/verify-opt-opt-store-badaddr-1`
  (`bash run.sh`; reduced.f90 is statement-minimal and register-pressure
  sensitive — do not trim further). Faulting insn `movsd %xmm2,(%rcx)` with
  %rcx=6 (an integer loop value): the unroll-remainder copy of the dependent
  loop `a0(i)=a0(i-1)+a1(i)` recomputes both load addresses but the store
  consumes %rcx, which no predecessor defines as an address — both `jmp`
  predecessors last write %ecx as integer scratch. `--emit-ir` at -O2 shows a
  correct in-block gep+store def, so the def is dropped in
  isel/regalloc/block-layout, not by an IR pass. With a mapped address this
  is silent memory corruption. Found by differential fuzzing (2/400 seeds;
  fz292 crashes with the same signature in a different loop shape). Adjacent
  to C36 (same `partial_remain_*` machinery, different failure). Fixture:
  `test_programs/ar2_unroll_remainder_store.f90` + a backend unit test that
  every vreg use in cloned remainder blocks has a dominating def.

- **C30 — dead-arg-elim strips parameters from address-taken internal
  procedures called through procedure dummies: every argument shifts one
  slot at -O1+. CRITICAL.** wrong-code (empty output/garbage/SIGSEGV varies
  with stack contents); -O1..-Ofast, correct at -O0. Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-proj-fgof-lineedit-opt-dead-arg-indirect-1`
  (`sh run.sh`; reduced.f90 prints 99 at -O0, 32765 at -O1). Root:
  `src/opt/dead_arg.rs` — `inst_uses_param` treats `GlobalAddr(..)` as not a
  use (line ~153), `internal_only` has no address-taken exclusion, and the
  rewrite loop patches only `Call(FuncRef::Internal, …)` sites, never
  `FuncRef::Indirect`. IR proof: at -O1 the callee loses its first param
  while the indirect call site still passes all args. Fix: treat GlobalAddr
  as address-taken (bail), or rewrite indirect sites too. Fixture:
  `test_programs/ar2_procdummy_dead_arg.f90`.

  ```fortran
  module m
    abstract interface
      subroutine provider_i(unused, val, out)
        integer, intent(in) :: unused, val
        integer, intent(out) :: out
      end subroutine
    end interface
  contains
    subroutine refresh(p)
      procedure(provider_i) :: p
      integer :: out
      call p(0, 99, out)
      print '(i0)', out    ! -O0: 99 ; -O1+: garbage
    end subroutine
  end module
  program main
    use m
    call refresh(prov)
  contains
    subroutine prov(unused, val, out)
      integer, intent(in) :: unused, val
      integer, intent(out) :: out
      out = val
    end subroutine
  end program
  ```

### Compiler core — high

- **C31 — unformatted WRITE of a derived-type ARRAY ICEs: "no register class
  for Array(Int(I8), 4)" at `src/codegen/x86/isel.rs:3046`. HIGH.**
  build-failure; all levels. Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-nondet-crash-unf-derived-array-ice-1`
  (`armfortas -O0 reduced.f90 …` → ICE, exit 4). Root:
  `fmt_push_whole_array` (`src/ir/lower/core.rs:32345`) — the per-element
  fallback does `load_typed(p, info.ty)` on aggregates; 4-byte aggregates
  miss the 8/16-byte register-class carve-outs the X64-O0-003/C6 fixes
  added. Same defect class, new producer: memcpy aggregates instead of
  loading by value. Sibling of C21 (the scalar case silently drops instead
  of ICEing). Fixture: `test_programs/ar2_unformatted_derived_array.f90`.

- **C32 — ICE (IR verification failure) when two sibling module procedures
  both have a dummy procedure named `f`, one via interface block and one via
  `procedure(iface)`. HIGH.** build-failure on conforming code; all levels.
  Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-abi-sema-procdummy-collision-ice-1`
  (`afs -O0 -c reduced.f90` → "integer op %16 has non-integer operand %12 :
  ptr<i8>" in `afs_modproc_qm_apply2`). Either procedure alone compiles;
  renaming the second dummy compiles; the later `procedure(iface) :: f`
  contaminates the earlier procedure's interface-block dummy. Root: sema
  dummy-procedure symbols keyed by bare name leaking across sibling scopes.
  Needs a double use (`f(a) + f(a)`) to trigger the verifier. Fixture:
  `test_programs/ar2_dummy_proc_name_collision.f90`.

- **C33 — deferred-length char array append `a = [a, scalar]` welds the old
  elements into one slot, doubles `len`, and emits heap-pointer bytes as
  string data. HIGH.** wrong-code + address leak; all levels. Repro:
  `verify-audit-scratch/verify-lang-lang-strarr-append-garbage-1`. Root: the
  descriptor-backed char-array assignment branch
  (`src/ir/lower/core.rs` ~45383) only handles constructors with an explicit
  constant type-spec; a spec-less constructor (`[a, 'cc  ']`, the natural
  deferred-len growth idiom) falls through to the generic non-char
  `lower_runtime_array_constructor_descriptor` + `afs_assign_allocatable`
  path, which conflates total byte count with element length. Distinct from
  fixed C6 and from the fpm x12 fixed-length append fix (that fixture
  passes). Fixture: `test_programs/ar2_deferred_char_append.f90`.

- **C34 — automatic character ARRAY with runtime length silently gets len 0
  and empty storage (the scalar form works). HIGH.** wrong-code; all levels.
  Repro: `verify-audit-scratch/verify-stress-stress-autochar-array-len0-1` (`./run.sh`).
  In `subroutine s(n)` with `character(len=n) :: arr(2)`, `len(arr(1))` is 0
  and assignments are dropped. Root: `src/ir/lower/alloc.rs` — the scalar
  path has a runtime-length branch (`CharKind::FixedRuntime`, ~760); the
  fixed-shape array path (~891) requires a constant `char_len` and otherwise
  falls to `(elem_ty, None, CharKind::None)` (~900-917), erasing the
  character length entirely. Fixture:
  `test_programs/ar2_auto_char_array_runtime_len.f90`.

- **C35 — access to a nonexistent derived-type component compiles silently:
  reads yield zero/empty, writes are dropped. HIGH.** missing diagnostic
  turning typos into silent runtime behavior; all levels (sema). Repro:
  `verify-audit-scratch/verify-proj-facsimile-proj-facsimile-missing-component-1`
  (`bash run_reduced.sh`; `print *, m%nope` prints 0; gfortran: "'nope' at
  (1) is not a member of the 't' structure"). Root: no sema validation of
  component existence; `Expr::ComponentAccess` lowering
  (`src/ir/lower/expr.rs` ~3656) falls back to `const_i32(0)` with a comment
  "fallback for unresolved component access" and no user-visible diagnostic.
  Fixture: `test_programs/ar2_unknown_component_rejected.f90`
  (ERROR_EXPECTED).

- **C36 — ICE at -O2/-O3/-Ofast after loop-unroll: partial-unroll remainder
  latch passes 1 arg to a 2-param header. HIGH.** build-failure; -O2/-O3/
  -Ofast (ok at -O0/-O1/-Os). Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-proj-fgof-lineedit-opt-loop-unroll-ice-1`
  (`sh run.sh` → "IR verifier failed after pass `loop-unroll`: branch from
  partial_remain_latch_16 to partial_remain_header_15: expected 2 args,
  got 1", exit 4). Trigger needs a loop inside a conditional whose trip
  count derives from `mod(<call>, N)`. Root: `src/opt/unroll.rs` remainder
  construction — `header_remain` gets an `acc` param when
  `shape.reduction.is_some()` (~1826) but `remain_new_acc` (~2013) can come
  back None, leaving the latch branch one arg short. Same machinery as C29.
  Fixture: `test_programs/ar2_unroll_remainder_reduction_ice.f90`.

- **C37 — -O2/-Os codegen is nondeterministic: identical invocations produce
  different assembly (regalloc choices, stack slots, label numbering); .o
  files differ. HIGH.** nondeterminism; -O2 and -Os (O0/O1/O3/Ofast stable
  on this source). Repro: `verify-audit-scratch/verify-proj-fgof-watch-opt-o2-nondet-asm-1`
  (`sh run.sh`: 8 compiles of fe.f90 → 3-5 distinct sha256; needs the
  companion `fgof_watch_types.f90`; not reducible below ~145 lines — the
  trigger is aggregate inlined-cluster size, and `cargo test
  determinism_sweep` over all 681 test_programs does not catch it). No wrong
  code demonstrated; breaks the project's own REPRO_CHECK/byte-determinism
  oracle at the two most-used levels. Likely a per-process HashMap/HashSet
  RandomState in an O2-and-Os-shared pass feeding an order-sensitive
  tie-break — possibly the same root as C42 (fix C42 first, re-measure).
  Fixture: add fe.f90-shaped source to the determinism_sweep corpus.

- **C38 — cubic compile-time blowup: `intent(out)` of a USE-imported derived
  type with a default-initialized derived-array component at -O1+; makes any
  optimized ferp build impossible. HIGH.** compile-time; -O1..-Ofast
  (-O0 unaffected: 0.02s vs ~20-28s at N=128; O(N³) in the component array
  extent). Repro:
  `verify-audit-scratch/verify-proj-ferp-toolchain-intent-out-defaultinit-cubic-1/reduced`
  (`./run.sh`, 21-line two-module file). Trigger requires all of:
  USE-imported type (same-module form ~8x faster), intent(out) dummy,
  default-initialized nested derived components, -O1+. -O0 IR is emitted
  instantly; the pass pipeline burns the time — suspect an optimizer pass
  over the unrolled intent(out) default-init reset (interacting with the
  `13644262`/`a816a7cf` default-init lowering). Real impact: ferp
  `regex_api.f90` (181 lines) exceeds 5 min at -O1/-O2. Related to but
  distinct from the tracked DSE blowup (see Known duplicates) — consolidate
  when triaging. Fixture: compile-time budget test (bencch), since the e2e
  annotation language has no wall-clock oracle.

  ```fortran
  module tm2
    implicit none
    type :: state_set_t
      integer(8) :: bits(17) = 0
      integer :: count = 0
    end type
    type :: entry_t
      integer(8) :: state_hash = 0
      type(state_set_t) :: next_states
      logical :: valid = .false.
    end type
    type :: opt_t
      type(entry_t) :: dfa_cache(128)
    end type
  end module
  module um2
    use tm2
    implicit none
  contains
    subroutine reset(x)
      type(opt_t), intent(out) :: x   ! -O0 0.02s, -O1 ~20s, O(N^3) in dfa_cache extent
    end subroutine
  end module
  ```

### Compiler core — medium

- **C39 — CSHIFT and EOSHIFT (F90) and PARITY/IALL (F2008) are absent from
  the intrinsic table; the diagnostic misleads ("variable 'cshift' used but
  not declared (IMPLICIT NONE is active)"). MEDIUM (merged: opt
  missing-cshift-eoshift-1 + lang cshift-eoshift-missing-1).** build-failure;
  all levels (sema). Repros: `verify-audit-scratch/verify-opt-missing-cshift-eoshift-1`,
  `verify-audit-scratch/verify-lang-lang-cshift-eoshift-missing-1`. `grep -rni cshift
  src/` is empty; `iall/iany/iparity/parity` appear only in a dead ir-lower
  table (`is_array_reducing_intrinsic`) that sema rejects before reaching.
  `sema/validate/core.rs::is_intrinsic_name` (~4173) is the gate. Also add a
  "recognized but unimplemented intrinsic" diagnostic class. Fixture:
  `test_programs/ar2_cshift_eoshift.f90`.

- **C40 — SIZE (and likely LBOUND/UBOUND/SHAPE) applied to a no-DIM
  MAXLOC/MINLOC result emits an unresolvable external `call @size` → link
  failure. MEDIUM.** build-failure; all levels. Repro:
  `verify-audit-scratch/verify-lang-lang-size-of-maxloc-1`. Root:
  `lower_array_location_dim_descriptor` (`src/ir/lower/core.rs:38760`)
  requires DIM= and returns None for the location-vector form; the `?` at
  ~48676 aborts `lower_array_intrinsic` and the caller synthesizes an
  external call instead of erroring at compile time. Fixture:
  `test_programs/ar2_size_of_maxloc.f90`.

- **C41 — CALL of a module FUNCTION is accepted silently (result discarded);
  gfortran hard-errors per F2018 15.5.1. MEDIUM.** missing diagnostic; all
  levels (sema). Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-proj-facsimile-proj-facsimile-call-of-function-1`
  (`./run_reduced.sh`). The L-tail subroutine/function form check covers
  intrinsics only (`Stmt::Call` at `sema/validate/core.rs` ~1907 gates on
  intrinsic resolution); user procedures are unchecked. Fixture:
  `test_programs/ar2_call_of_function_rejected.f90` (ERROR_EXPECTED).

- **C42 — LoopUnswitch picks its candidate by HashSet iteration order:
  nondeterministic compilation at -O2/-O3/-Os/-Ofast. MEDIUM.**
  nondeterminism; O0/O1 deterministic. Repro:
  `verify-audit-scratch/verify-proj-fuss-nondet-unswitch-hashset-1` (`bash
  run_reduced.sh`: 6 compiles → 2 distinct assembly hashes, split varies per
  process = per-process RandomState). Root:
  `src/opt/unswitch.rs:163-193 find_unswitch_candidate` iterates
  `NaturalLoop.body: HashSet<BlockId>` (`src/ir/walk.rs:552`) and returns
  the first invariant CondBranch; the sibling `clone_loop`
  (`src/opt/loop_utils.rs:83`) already sorts — the omission is an oversight.
  ~2-line fix (sort by block id). Then re-measure C37. Fixture: unit test on
  candidate ordering + a REPRO_CHECK program with two invariant conditionals.

### Compiler core — low

- **C43 — SIGN() ignores negative real zero although the processor
  distinguishes −0.0: lowered as `b >= 0` select instead of sign-bit copy.
  LOW.** all levels. Repro: `verify-audit-scratch/verify-opt-sign-negzero-1`. Root:
  `src/ir/lower/intrinsic.rs:514-552` (`fcmp Ge` on B; IEEE −0.0 ≥ 0.0 is
  true). Fixture: `test_programs/ar2_sign_negzero.f90`.
- **C44 — parameterized derived types (F2003) do not parse:
  `type(vec(n=3)) :: v` → "parse error: expected ), got (". LOW.** Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-lang-lang-pdt-parse-1`.
  `src/parser/decl.rs::parse_type_or_class_spec` (281-302) has no
  type-param-spec-list path. Loud, clean rejection — fine per policy, but
  track the feature gap. Fixture: `test_programs/ar2_pdt_parse.f90`
  (ERROR_EXPECTED until implemented).
- **C45 — character runtime ops 11-27x slower than gfortran: TRIM operands
  are materialized as heap copies (afs_allocate+memcpy+afs_deallocate per
  operand per loop iteration) despite the runtime's TRIM-returns-a-view
  contract; len_trim/compare are byte-at-a-time. LOW (perf).** all levels
  (opt-independent). Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas-verify/verify-proj-fuss-perf-char-runtime-1`
  (`bash run_reduced.sh`; `trim(a)==trim(b)` in a 4M loop: gf 0.03s, afs
  ~1.0s; 8 calls incl. 2 malloc/free pairs per iteration in the -O2 asm).
  Real-world: `fuss -p` 11x slower on a 6000-dirty-file repo. Fix in TRIM
  lowering (pass ptr+len view, hoist invariants) + `runtime/src/string.rs`.
  Fixture: bencch runtime benchmark.
- **C46 — every .amod stamps `# abi: arm64-apple-darwin` and
  `@abi cc=aapcs64` with x0-x7 assignments regardless of compile target.
  LOW.** `src/sema/amod.rs:246,~1083` hardcode the strings; `write_amod`
  takes no TargetSpec (caller at `driver/mod.rs:1725` has `opts.target` in
  scope). Currently write-only metadata (parse_amod discards it), but it
  violates the TargetSpec-threading rule and poisons any future cross-target
  ABI check. Repro: `verify-audit-scratch/verify-nondet-toolchain-amod-abi-stamp-1`.
  Fixture: unit test asserting the stamp matches `opts.target`.

### Runtime library

- **R1 — formatted L editing of any non-default logical kind
  (logical(1)/(2)/(8)/c_bool) silently drops the ENTIRE output record;
  list-directed prints ` 1` instead of ` T`. HIGH.** io-divergence; all
  levels. Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-abi-io-logical-kind-record-drop-1`
  (`bash run_reduced.sh`). Root chain: `fortran_type_to_ir_scalar_type`
  (`src/ir/lower/core.rs:55347`) maps only kind-4 logicals to `Bool`; kinds
  1/2/8 become Ints and are pushed as `IoValue::Integer`;
  `runtime/src/format.rs` (~1003) has no (Logical, Integer) arm →
  `FormatError::TypeMismatch`; `afs_fmt_end`
  (`runtime/src/io_system.rs` ~4213) sets io_status=1 and drops the record —
  and PRINT without IOSTAT= silently discards the status (verified: with
  `iostat=ios` the runtime reports ios=1). Fix both ends: push non-default
  logicals as logical, and make an unhandled I/O error terminate loudly.
  (An earlier finder claim that the corruption cascades to the next
  statement was refuted — each record fails independently.) Fixture:
  `test_programs/ar2_logical_kind_format.f90`.

- **R2 — NAMELIST READ silently assigns nothing and reports iostat=0.
  HIGH.** io-divergence; all levels. Repro:
  `verify-audit-scratch/verify-lang-nml-read-noop-1` (`bash run.sh`). Two independent
  parser bugs in `runtime/src/io_system.rs`: (1) `split_namelist_fields`
  (:2648) splits only on commas, never blanks/newlines — gfortran's own
  multi-line no-trailing-comma style garbles the first field; (2) the
  array-continuation gate (:2574) tests `data_type == 2` ("fixed string")
  instead of is-array, so integer/real arrays never fill past element 1 even
  with commas. iostat is unconditionally forced 0 (:2432). README claims
  NAMELIST I/O supported; there is zero e2e coverage of namelist READ.
  Fixture: `test_programs/ar2_namelist_read.f90`.

- **R3 — WRITE → REWIND (or POS=) → READ on the same open named unit fails
  (iostat=1, garbage variable); without IOSTAT the failing READ does NOT
  terminate the program — blank data, exit 0. HIGH.** io-divergence; all
  levels. Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-lang-lang-rw-rewind-fail-1`
  (`bash run_reduced.sh`). Root: OPEN without ACTION= maps STATUS=
  'replace'/'new' to effective action "write" (`runtime/src/io_system.rs`
  ~646-724) — sequential units get a write-only `BufWriter` stream and
  stream/direct units a write-only fd; REWIND happily seeks, the READ then
  fails at stream level. Adding `action='readwrite'` fixes it, isolating the
  default. Second defect stacked on top: an unhandled READ error must
  error-terminate, not return silently. `status='scratch'` units already
  default readwrite. Fixture: `test_programs/ar2_write_rewind_read.f90`.

- **R4 — formatted READ from a terminal in cbreak mode never returns:
  non-advancing `(A1)` read blocks for newline/EOF; interactive TUIs are
  dead (fuss). HIGH.** hang; all levels. Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-proj-fuss-hang-tty-cbreak-read-1`
  (`bash run_reduced.sh`; gfortran echoes each keystroke, afs rc=124).
  Root: `afs_fmt_read_string_noadvance` (`runtime/src/io_system.rs` ~4810)
  calls `read_line()` when no record is in flight; for Stdin that is
  `stdin().lock().read_line()` — line-buffered regardless of advance='no'.
  Affects Stdin, FileRead, and FileRaw alike; non-advancing reads need a
  bytewise path. Fixture: PTY-driven case in the bencch runtime suite (the
  e2e annotation language has no tty oracle).

- **R5 — list-directed output inserts a blank separator between adjacent
  CHARACTER values (F2018 13.10.4: none). MEDIUM (merged: proj-sniffert
  io-listdir-char-adjacent-sep-1 + proj-facsimile listdir-char-sep-1).**
  io-divergence; all levels. Repros:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-proj-sniffert-io-listdir-char-adjacent-sep-1`,
  `verify-audit-scratch/verify-proj-facsimile-proj-facsimile-listdir-char-sep-1`.
  `print *, 'a','b','c'` → ` a b c` instead of ` abc`; corrupts the
  ubiquitous `print *, 'label: ', trim(x), ' suffix'` idiom. Root:
  `afs_write_string` (`runtime/src/io_system.rs:999`) writes an
  unconditional leading blank with no previous-item-was-character state.
  Fixture: `test_programs/ar2_listdir_char_adjacent.f90`.

- **R6 — IOMSG= on OPEN is never assigned on error (iostat correct, message
  variable untouched) — F2018 12.11.6 violation. MEDIUM.** all levels.
  Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-proj-ferp-io-iomsg-never-assigned-1`
  (`sh reduced_run.sh`). Root spans both sides: the OPEN lowering
  (`src/ir/lower/stmt.rs` ~7600-7836) never looks up an `iomsg` spec, and
  `OpenControlBlock` (`runtime/src/io_system.rs`) has no iomsg fields — the
  keyword is silently swallowed. READ/WRITE iomsg plumbing exists
  (db9ee6af/92770dd0); extend it to OPEN (and audit CLOSE/INQUIRE/WAIT).
  Fixture: `test_programs/ar2_open_iomsg.f90`.

- **R7 — X/1X positioning at end of record emits a spurious trailing blank
  (X must position, not write). LOW.** all levels. Root:
  `runtime/src/format.rs` ~740, `FormatDesc::Skip` eagerly pushes spaces
  instead of moving a column cursor; a record must end at the highest
  position actually written (F2018 13.8.1.2). Breaks byte-exact diffs for
  every `(n(i0,1x))` idiom. Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-abi-io-trailing-x-blank-1-mine`.
  Fixture: `test_programs/ar2_trailing_x_record.f90`.
- **R8 — Fw.0 editing omits the required decimal point (`  1.` → `   1`).
  LOW.** Root: `format_fixed` uses Rust `{:.0}`, which never emits the
  trailing point (F2018 13.7.2.3.2 requires it). Repro:
  `verify-audit-scratch/verify-lang-lang-fw0-decimal-1`. Fixture:
  `test_programs/ar2_f_edit_zero_d.f90`.
- **R9 — kP scale factor on E editing keeps d fractional digits instead of
  d−k+1 (`2PE12.4` → ` 12.3457E+03`, want `  12.346E+03`). LOW.** Root:
  `format_e_style` (`runtime/src/format.rs` ~1140) adjusts exponent/mantissa
  but formats with unadjusted `decimals`. Repro:
  `verify-audit-scratch/verify-lang-kp-scale-digits-1`. Fixture:
  `test_programs/ar2_kp_scale_e_edit.f90`.
- **R10 — G0 prints real(8) with 8 significant digits (gfortran: 17,
  round-trip). LOW.** Root: `format_g0` hardcodes 9 digits;
  `IoValue::Real(f64)` carries no kind, so the formatter cannot pick 9 vs
  17. Thread the kind through. Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-proj-fortbite-g0-real64-digits-1`.
  Fixture: `test_programs/ar2_g0_real8_roundtrip.f90`.
- **R11 — character STOP code is silently dropped (`stop "msg"` emits
  nothing anywhere; integer STOP sets the exit status but prints no banner).
  LOW.** Root: `Stmt::Stop` lowering (`src/ir/lower/stmt.rs` ~5898) discards
  `code_expr` when it is character-typed; ERROR STOP has the working
  `StopCode::Msg`/`afs_error_stop_msg` path to mirror. Repro:
  `verify-audit-scratch/verify-proj-fgof-termios-rt-stop-charcode-silent-1`. Fixture:
  `test_programs/ar2_stop_char_code.f90` (STDERR_CHECK).
- **R12 — filenames with non-UTF8 bytes are lossy-converted (U+FFFD) on
  OPEN: staging under a Latin-1 directory fails iostat=2 though the dir
  exists. LOW.** Root: `fortran_file_name` → `String::from_utf8_lossy`
  (`runtime/src/io_system.rs:1882-1893`). Same root-cause family as the
  tracked fgof-fs F2 "CHARACTER not byte-transparent" item
  (`verify-proj-fgof-fs-rt-char-not-byte-transparent-1`) — fix together
  with raw-byte passthrough. Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-proj-fgof-temp-proj-fgof-temp-latin1-parent-dir-1`.
  Fixture: Rust-side runtime unit test (byte-transparent path round-trip).
- **R13 — formatted sequential I/O 2-6x slower than gfortran: every
  formatted WRITE forces a full BufWriter flush. LOW (perf).** Root:
  `afs_write_newline` (`runtime/src/io_system.rs` ~1030) calls `u.flush()`
  per record, defeating buffering (1M writes ≈ 1M flushes). Read side is a
  milder 1.5-2x. Repro: `verify-audit-scratch/verify-proj-fit-perf-fmt-seq-io-1`.
  Fixture: bencch runtime benchmark.

### Driver

- **D1 — (REGRESSION, listed above under Regressions).**
- **D2 — `.amod` written non-atomically (`fs::write` truncate-in-place,
  `src/driver/mod.rs:1750`) AND the reader accepts any truncated prefix as a
  valid module: parallel builds flakily fail with a misleading diagnostic or
  silently miscompile. HIGH.** nondeterminism/wrong-code; all levels. Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-nondet-nondet-amod-torn-1`
  (`./reduced_run.sh` — deterministic truncation demo; `race.sh` — live
  2-writer/4-reader race, fired 1/480 reader compiles). A header-only .amod
  parses as "a module exporting nothing"; with implicit typing the consumer
  compiles rc=0 and prints uninitialized garbage. The fnv1a checksum header
  is stored but never compared anywhere (`grep "\.checksum"` outside
  amod.rs: empty), and it hashes the source .f90, not the .amod bytes, so it
  could not detect tearing anyway. Fix: temp-file + atomic rename on write;
  end marker or self-checksum + hard reject on read. Fixture: driver/amod
  Rust unit tests (torn-file rejection).
- **D3 — AFS_LD=1 routing on ELF is dead on arrival: `elf_link_args`
  unconditionally passes `--gc-sections` (and `-pie` by default), which
  afs-ld's ELF mode rejects; no flag suppresses --gc-sections, so the
  advertised drop-in routing can never link. MEDIUM.** loud failure, opt-in
  path only; opt-independent. Repro:
  `verify-audit-scratch/verify-toolchain-driver-afsld-routing-1` (`bash run.sh`).
  Root: `src/driver/elf_crt.rs:242-246` vs afs-ld's flag parser (rejects
  unknown dash-flags). The x16 comment at `src/driver/mod.rs:2020-2024`
  claims the routing is honored; no test anywhere exercises AFS_LD=1 to a
  successful ELF link. Fixture: `tests/elf_link_e2e.rs` success-path test
  once L10 lands.

### afs-ld

- **L10 — afs-ld cannot link any Fortran executable on x86_64-linux-gnu:
  dynamic mode has no archive support (`link_dynamic` lacks the AR_MAGIC
  branch `link_static` has), `-lfoo` resolves only `.so` with no `.a`
  fallback, and static mode synthesizes no `__ehdr_start` and rejects
  glibc's libm.a linker script. MEDIUM.** Gated behind the D3 rejection, so
  unreachable in practice today; the runtime ships only as an archive, so
  every blocker is fatal once D3 is fixed. Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-toolchain-afsld-glibc-exec-1`
  (`bash run.sh`; GNU ld links and runs the identical inputs). Static-glibc
  (`__ehdr_start`) is out of the x11 musl scope — the archive-in-dynamic-mode
  and `.a`-fallback gaps are the actionable rung. Fixture: afs-ld regression
  tests mirroring `archive_member_selection_links_only_used_members` for the
  dynamic path.
- **L11 — afs-ld resolves cross-archive backward references GNU ld rejects:
  archives resolve to a global fixed point (implicit --start-group), and the
  group markers are silently no-ops. LOW.** Deliberate design
  (`elf.rs link_static` doc comment), but a real drop-in-contract
  divergence: links that only succeed under afs-ld break under GNU ld, and
  ambiguous multi-archive member selection can differ. Repro:
  `verify-audit-scratch/verify-toolchain-afs-ld-archive-rescan-1` (`bash run.sh`; GNU ld
  errors "undefined reference to g", afs-ld links, exit 35). Distinct from
  fixed L3 (definition-order selection). Fix option: honor strict
  single-pass unless grouped, or document + honor/reject the markers loudly.
  Fixture: afs-ld test with the 3-object/2-archive shape, both orders.

### afs-as

- **A7 — `.space`/`.skip` fill-byte operand silently ignored; zeros emitted
  where gas emits the fill (`.space 4,0x90` → `00 00 00 00`). MEDIUM.**
  Silent wrong bytes in .text (NOP sled becomes `add %al,(%rax)`) and
  .data; same class as fixed A1-A4. `.fill` is loudly rejected —
  asymmetric. Root: `afs-as/src/x86/parse.rs:518-525` parses only
  `args.split(',').next()` and lowers all three directives to
  `Directive::Zero`. Compiler codegen never emits a fill operand, so
  exposure is hand-written/third-party .s only. Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-toolchain-afs-as-space-fill-1`
  (`bash run_reduced.sh`, 3-line .s). Fixture: afs-as differential test.
- **A8 — `\a` in `.ascii`/`.asciz` emits BEL (0x07) where gas emits the
  literal `a` (0x61). LOW.** gas has no `\a` escape (unknown escapes drop
  the backslash); the f8f5d50 "gas-parity string escapes" commit added it in
  both `afs-as/src/x86/parse.rs:578` and `afs-as/src/lex.rs:438`. All other
  escapes (octal, \x hex, \b\f\v\r\n\t\"\\, embedded NUL) verified
  byte-identical. Repro:
  `/home/mfwolffe/GithubOrgs/FortranGoingOnForty/armfortas/verify-toolchain-afs-as-escape-a-1`.
  Fixture: afs-as differential test.

## Known duplicates

Mapped to already-tracked issues during verification (not counted above):

- **DSE alias-query compile-time blowup, ~O(N³) on N stores to one array at
  -O1+** (`stress-dse-alias-o2-blowup-1`) — this IS the open "superlinear
  compile time on 500+/1400-line modules" item (noted_items.md:650,
  model-drift audit real-project sweep, sniffert `treemap_layout.f90`). The
  stress finder supplied what the tracked item lacked: the mechanism
  (`src/opt/dse.rs:47-66` grows an unbounded `pending` list → O(N²)
  `alias::query` calls, each O(N) via the unindexed `find_inst` scan in
  `src/opt/alias.rs:255`) plus a reduced repro
  (`verify-audit-scratch/verify-stress-stress-dse-alias-o2-blowup-1`). Fix the tracked
  item with this diagnosis; note the facsimile **-O0** stall (590 s
  re-measured on `editor_state_module.f90`) cannot be this mechanism and
  remains unexplained (see Inconclusive). C38 is a distinct trigger in the
  same symptom class — triage together.
- **Array constructor of deferred-length allocatable character takes element
  length from a same-named local in a SIBLING internal procedure**
  (`sema-char-ac-sibling-len-1`) — this is the tracked RESIDUAL from the
  memmove split (noted_items.md, "watch test_watch_filters … assertion-level,
  needs its own reduction"). The finder supplied the reduction (18 lines, in
  the fgof-watch verify dir under `verify-audit-scratch`): silent truncation to the
  sibling's unrelated length at every opt level. Upgrade the tracked item's
  severity — it is silent wrong-code, not "lower severity".

Known-open items re-confirmed by finders and deliberately not re-reported:
fgof-fs F2 (runtime CHARACTER not byte-transparent; R12 is a new
manifestation of the same root) and F3 (READ END= branch not taken);
sniffert `bind(c,name=)` collision and the cross-module recursive-dealloc
hang (both still reproduce; the audit doc's "superlinear compile" framing for
treemap_layout is wrong — it is the dealloc hang); list-directed integer
field width divergence (`verify-proj-fgof-temp-io-listdir-int-width-1`); the
defop-scan-quadratic and usechain-lower-quartic compile-time items (verify
dirs in repo root); multi-file `-c` object placement (campaign log).

Positive drift to record in the ledgers: the **ferp `[0-9]` digit-range
regex miscompile no longer reproduces** (open FAIL in the model-drift audit —
flip to FIXED with a fixture; the 118-case grep-comparison suite now passes),
and the **fgof-process residual error-stops** (test_parent_state/
test_run_options/test_validation) no longer reproduce — flip in
noted_items.md.

## Refuted

Three filed findings did not survive verification and are excluded. Two
sub-claims inside confirmed findings were also corrected: the R1 "formatter
corruption cascades to the following statement" claim (records fail
independently), and the L10 headline framing (afs-ld is opt-in; the default
ELF pipeline uses system ld and is unaffected).

## Inconclusive — needs a human look

- **C37 vs C42:** possibly one root cause (both O2-family HashMap/HashSet
  ordering). Land the C42 two-line sort, then re-run the C37 8-compile hash
  check before opening a second workstream.
- **facsimile -O0 compile stall** (`editor_state_module.f90`, 590 s at -O0):
  not explained by the DSE mechanism (no passes run at -O0) nor by C38 (also
  -O1+-gated). Needs profiling; it currently blocks any armfortas build of
  facsimile.
- **fuss -O3 determinism flap:** one 6-sample batch stable, another
  nondeterministic. Needs longer sampling once C42 lands.
- **Stray stub `.mod` emission:** armfortas writes a fake ASCII `<mod>.mod`
  beside the real `.amod`; gfortran in the same directory picks it up ahead
  of its own `-J` dir and fails "not a GNU Fortran module file". Observed
  during C26 verification; mixed-compiler build dirs are a real workflow.
  Needs its own triage (drop the stub or write gfortran-ignorable content).
- **lang finder's t35 probe** (derived-array reversed-section
  self-assignment) reproduced against the 15:35 binary, which predates
  `606b54f8`; re-test on a current build before filing — it may be covered
  by C18's fix anyway.
- **DATA-init of COMMON members dropped** (nondet finder's p8 probe): likely
  fixed by C16+C17 together, but verify the combination explicitly — the two
  skips are in different functions.

## Recommended fix ladder

1. **Aliasing/self-assignment arc — C13 (regression), C18, C20** as one
   change: a single snapshot discipline for scalar-derived assignment,
   section assignment (derived+char), and the sret-descriptor path. C13
   first; it is a crash regression shipping in `ar-remediation`.
2. **Section/descriptor ABI — C14, C15:** copy-in/copy-out packing for
   non-contiguous actuals; stride-preserving pointer-assignment descriptors.
   C14's negative-stride form is memory corruption.
3. **Character subsystem sweep — C19, C23, C24, C25, C33, C34** (+ C45
   perf): the `char_kind` dispatch in `lower_array_assign`, char AC lengths,
   char MAX/MIN, char EQUIVALENCE, runtime-length char arrays. One reviewer
   should own the whole set; the holes are adjacent.
4. **Legacy storage — C16, C17:** COMMON array members and whole-array DATA.
   Bread-and-butter F77; both are silent.
5. **Derived-type I/O — C21, C22, C31** + the read-side record-length
   enforcement, and kill the `afs_fmt_end` silent swallow (shared with R1):
   unhandled I/O errors must terminate loudly.
6. **Optimizer/backend — C29, C30, C36** (wrong-code/ICE), then **C42 →
   re-measure C37** (determinism), then **C38 + the DSE known-dup**
   (compile time).
7. **Module system — C26 (ONLY-filter leak), D2 (atomic .amod + integrity),
   C32 (sema scope leak ICE).**
8. **Driver/ledger — D1:** merge `82c947e5` + its test; audit campaign-log
   FIXED entries for trunk ancestry (T5 process rule).
9. **Runtime I/O — R2, R3, R4** (namelist read, OPEN action default,
   non-advancing tty read), then R1/R5/R6, then the R7-R13 tail.
10. **Toolchain — A7; L10 archive-in-dynamic-mode + D3 flag surface**
    together (they gate each other); L11/A8/C46 as scheduled.

## Coverage appendix — what was and was not attacked

Per finder, condensed from coverage notes. The NOT-COVERED lists are the next
audit's work-list. Common caveats: shared 16-core box (~13 concurrent
agents), so all timing claims were re-measured; one transient tmpfs-full
event; the release binary under test was built 15:35 with the branch moving
underneath — self-assignment-family findings were re-confirmed on rebuilds.

- **abi:** attacked char-len ABI (fixed/deferred/allocatable, hidden-len
  ordering, keyword reordering), VALUE dummies all kinds, OPTIONAL/PRESENT
  chains, CLASS actuals + select type, derived intent(out)/results/
  move_alloc, assumed-shape/-size descriptors, generic dispatch cross-TU,
  BIND(C) both directions against real gcc objects (c_ptr/c_funloc round
  trips, callbacks), mixed-opt TU combinations. NOT covered: submodule
  clusters, unformatted/stream cross-TU, READ-side ABI, character(kind=4),
  BIND(C) struct-by-value caller side (known C2 residual), coarrays,
  DO CONCURRENT locality, IEEE modules, ENTRY, assumed-rank,
  recursion-depth stress, AFS_LD path.
- **opt:** attacked C1-regression mutations, legal aliasing (TARGET/
  EQUIVALENCE/COMMON vs LICM), loop-carried deps, IV edges, EXIT/CYCLE under
  unroll, CSE vs impure functions, SELECT CASE, unswitch, LICM past guarded
  division, NaN/Inf at -Ofast, FORALL/DO CONCURRENT, array intrinsics,
  strided sections, inline/IPO semantics (SAVE, optional, elemental,
  recursion); ~1200 fuzzed programs × 6 levels; SSE2 ceiling on emitted asm.
  Non-findings: UB DO-loop IV-overflow hangs (gfortran also); SUM 1-2 ulp
  accumulation-order diffs; lax LOGICAL-under-I edit acceptance. NOT
  covered: complex arithmetic under the vectorizer, unformatted/stream I/O
  paths, polymorphic reallocation (being reworked mid-audit), coarrays,
  IEEE rounding modes, cross-TU optimization, arm64.
- **lang:** attacked allocatable char growth, MOVE_ALLOC, auto-realloc, host
  association, cross-module generics, elemental defined operators, defined
  assignment, SELECT TYPE hierarchies, array constructors, EQUIVALENCE/
  COMMON, NAMELIST round-trip, internal/non-advancing/stream I/O, format
  descriptor families, zero-length/zero-size edges, TRANSFER, vector
  subscripts, recursion+SAVE, statement functions, fixed-form, SELECT CASE
  ranges, ASSOCIATE/BLOCK, procedure pointers, INQUIRE. Deliberately skipped
  (covered by prior round's verify dirs): FINAL ordering, WHERE/FORALL
  dependent RHS, MAXLOC MASK/BACK, DATA implied-do, intent(out)
  default-init. NOT covered: user-defined derived-type I/O (pointless until
  C21), coarrays, C-interop, submodules, UCS-4, REAL(16) (known
  loud-reject). Binary predated five remediation commits — re-run findings
  on a fresh build.
- **stress:** attacked register pressure (64 live accumulators), FP across
  calls, frame-size audits (bug-B regression absent), deep recursion, 1MB
  locals, 64-arg calls, hidden char lengths, allocation churn (no leak),
  1M+ record formatted/stream/list-directed I/O, overlapping
  self-assignments at 80KB, 2048² transposes, matmul, masked reductions,
  derived deep-copy churn, byte-determinism (serial + 6-way concurrent),
  5k-line procedures, 500-deep nesting, 5000-param modules, 1000-case
  SELECT, 100k-element constructors. NOT covered: >2GB arrays / >2^31
  elements, fixed-form stress, COMMON/EQUIVALENCE at scale, compiler
  peak-RSS, afs-ld stress, PTY/timing tests.
- **nondet:** attacked 300-compile object determinism, env sensitivity
  (paths, locale, CWD), 8-way parallel compiles, multi-file .amod graphs,
  runtime determinism under ASLR (setarch ±R) incl. an uninit-hunt battery,
  .amod determinism, diagnostics ordering, --emit-ir/-S stability. Skipped
  as sibling-covered: derived default-init misses, empty reductions, COMMON
  arrays. NOT covered: fpm stage0-3 bootstrap re-run, afs-ld byte
  determinism on this host (blocked by D3), full 679-file determinism sweep
  (sampled 15), arm64 objects, namelist, PATH-order toolchain substitution.
- **toolchain:** attacked full-corpus assembler differential (~560 programs
  × 4 levels: zero real divergences; known-benign NOP-fill ordering), ~230
  hand-written encoder probes (imm/ModRM/SIB/REX/SSE2/xmm8-15 corners),
  rel8/rel32 relaxation boundaries, A1-A6/L1-L4/L6-L9/T1 regression re-tests
  (all hold), adversarial links (duplicate strong, archive member refs,
  alignment, 1200-section objects, CFI objects). Premise correction: the
  default x86 pipeline is afs-as + system ld; afs-ld only under AFS_LD=1
  (currently unreachable, D3/L10). NOT covered: gdb backtrace truthfulness
  (no .cfi emission, -g is a loud no-op), -Os corpus level, random fuzzing
  of afs-as, TLS/PIC reloc families, Mach-O/arm64, AFS_AS=0 whole-binary
  differential.
- **proj-fortbite:** full build + suite (identical outcomes to gfortran,
  byte-identical logs), 110-line REPL differential deck, 6-level rebuild
  (byte-identical), determinism ×3, parser stress. Project bugs exonerated:
  gfortran-built fortbite segfaults in free_ast; stale e2e fixture. NOT
  covered: matrix det/inv/solve paths (blocked by C26's crash AND the
  project's own parser), fpm build route, defined-operator/defined-I/O .amod
  leak variants (recommended for C26's fix matrix), long-run REPL memory.
- **proj-ferp:** full default builds both compilers; all four suites (72/72,
  134/136 with identical failures, 125/125, 118/118); 2400 differential fuzz
  cases (BRE/ERE/PCRE, files to 10MB) — zero divergences; recursive search,
  200k-line streams, error paths, 20-run determinism; mixed-opt binary (all
  -O2 except the two C38-blocked files at -O0) green. `[0-9]` regression
  re-test: FIXED. NOT covered: pure -O2+ builds of the two C38-blocked files
  (impossible until C38), -fopenmp config, benchmark timing beyond one
  investigated case (~2.4x at -O0, unreported).
- **proj-fit:** 51/51 suite both compilers; 6-level opt-invariant sweep
  byte-identical; 16 adversarial inputs × 3 modes (1MB lines, CRLF, UTF-8,
  NUL bytes, marker edge cases) all match; CLI error paths match; 20-run
  determinism; TUI view/scroll code driven via a custom batch driver
  (byte-identical); -O2 asm spot check clean. Excluded as project bugs: OOM
  on 20k conflicts (quadratic by design, both compilers), UB on malformed
  conflicts (both crash). NOT covered: interactive TUI event loop, raw
  keyboard/ioctl paths, fpm build, valgrind (absent).
- **proj-fuss:** modular + legacy builds at 6 levels (output-identical);
  `-p`/`-p -a` byte-identical across 15+ repo states incl. UTF-8/quoted
  names, 3500-entry repos, merge conflicts, detached HEAD; 10-run
  determinism; TUI first frame under pty byte-identical; newline-terminated
  TUI session transcripts identical. NOT covered: raw-mode interactive TUI
  beyond frame 1 (hard-blocked by R4), mutating TUI actions (commit/stash —
  key delivery racy under both compilers), read_key_with_timeout paths. No
  perf/strace/valgrind on box; ptrace_scope=1.
- **proj-sniffert:** build-only rung FAILS as-shipped on the KNOWN
  cross-module recursive-dealloc hang (reconfirmed) and the KNOWN
  bindc-name-collision (empty scan tree). After patching both: tree-scan
  debug logs byte-identical on adversarial trees (40-deep, 300-entry dirs,
  unicode/symlink/chmod-000, 5MB files), interactive pty navigation and
  delete flow byte-identical, O0/O2/O3/Ofast identical, 3× determinism,
  examples identical. NOT covered: real deletion via trash/rm, terminal
  resize/color, hello_ncurses, fpm build, object-level diffing.
- **proj-facsimile:** reference gfortran build OK (2 of 8 unit tests
  segfault under gfortran itself — project bugs). armfortas rung FAILS on
  the known editor_state compile stall (-O2 AND -O0 >590 s — worse than
  documented; see Inconclusive). Salvage: every other module compiles except
  syntax_highlighter (C27); 7 unit-test programs pass with
  gfortran-identical semantics; 5 of them byte-identical across 6 levels;
  gap-buffer/JSON/regex/tokenizer differential drivers identical. NOT
  covered: the fac binary end-to-end, pexpect integration, live LSP,
  termios/PTY runtime, fixed-form.
- **proj-fgof-fs:** fpm build+test green at ALL SIX levels both compilers;
  ~2200-line path-string differential, filesystem differential (walk/stat/
  copy/move/remove/which), edge fixtures (5GiB sparse, NAME_MAX, dangling
  symlinks, 0xE9 names through the C-interop scandir path — byte-intact),
  object determinism, 126 test binaries ×3 runs. Known F2/F3 re-confirmed
  open. NOT covered: fpm profile flag dialects, parallel invocation stress,
  valgrind, arm64, fypp consumers.
- **proj-fgof-process:** fpm build+test green both compilers — the
  noted_items residual failures NO LONGER REPRODUCE (flip the entry). 30+
  differential stress scenarios (argv/shell, 128KiB pipes, env, cwd,
  timeout kills, exit codes, 100 spawns, zombie check, high-byte capture
  transparency); suites re-run at -O2/-O3/-Os/-Ofast; determinism ×5. NOT
  covered: CMake path, valgrind, long-run soak, signal-mask probing,
  whether the C25 char-AC bugs affect PARAMETER/init contexts (only
  assignment and argument contexts probed).
- **proj-fgof-temp:** fpm build + 9/9 suite both compilers incl. -Ofast;
  byte-exact atomic writes (1 MiB, empty, 50× overwrite), all-256-byte
  transparency through the unformatted-stream path, guard partial-failure
  accounting, 3000/9000-entry scale, rename-onto-dir, 4095/4096/4097
  boundaries, 10 serial + 6 concurrent determinism runs, compile-time
  parity. LOW note: guard registration constant-factor ~4x (library is
  O(n²) by design). NOT covered: ENOSPC/EACCES fault injection, CMake,
  signal-interrupted cleanup, TMPDIR unset edge, arm64. C20 was re-verified
  against fresh worktree builds at HEAD.
- **proj-fgof-lineedit:** fpm build+test green at all 6 levels; 5000- and
  30000-step action-fuzz drivers over the full API diffed against gfortran
  at all levels; history/large-buffer stress; edge-case driver (omitted
  optionals, zero-size menus); object reproducibility at -O2. Library
  itself is clean — both findings (C30, C36) are compiler bugs exposed by
  consumer-shaped drivers. Lesson recorded: the first fuzz "divergence" was
  UB in the fuzzer's own LCG. NOT covered: CMake path, valgrind, asm
  eyeballing, compile-time scaling, arm64.
- **proj-fgof-watch:** gfortran reference 8/8 green; armfortas 6/8 — both
  divergences root-caused and reduced (C13 regression; known-dup
  sibling-len residual). 6 passing tests rebuilt at -O0/-O2/-O3 green;
  200-file create/modify/remove/move differential byte-identical and
  deterministic; compile time fine; object reproducibility check found C37.
  NOT covered: -Os/-Ofast full-suite runs, fpm --profile release, valgrind,
  arm64, deeper C-interop, whether C37's variants can be functionally wrong
  (all observed variants passed the suite).
- **proj-fgof-termios:** gfortran 5/5 with real PTYs; armfortas 5/5 at all
  six levels; 100-execution flake loop clean; examples under real PTY
  identical; adversarial stress driver (400 bind/raw/cbreak cycles with
  C-side termios verification, winsize edges, 20k-iteration allocatable
  churn) byte-identical at all levels; 10-run determinism; byte-identical
  double compile; O2 asm eyeball sane. NOT covered: signal delivery in raw
  mode, controlling-terminal revocation, PTY exhaustion, valgrind, arm64,
  CMake path.
