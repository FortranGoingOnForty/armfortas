# x86_64 real-project campaign log (x12)

Differential campaign: real Fortran projects built and tested under
armfortas on x86_64, diffed against gfortran on the ELF targets. Driven
by `cargo run -p afs-tests -- projects run` against
`bencch/projects/ecosystem.afproj`. Builds are direct compiler
invocations (no fpm/CMake/meson masquerade) so armfortas and gfortran
run the identical command — a fair diff, no cheating.

Reference compilers: gfortran on ELF (Linux/FreeBSD), flang-new on
macOS. Host for these runs unless noted: dorado (FreeBSD 15 x86_64),
gfortran 14.2.0, armfortas release build.

Ladder (smallest dependency surface first):
1. test-drive — green
2. toml-f — green: x86_64 215/215 and macOS arm64 215/215 (both match gfortran)
3. stdlib — pending (gated on l07–l09, now landed)
4. fpm — pending (self-hosting)
5. fortsh — pending

## XFAIL / divergence ledger

No divergences in state "observed but unclassified". (Empty so far —
test-drive matched gfortran exactly.)

## FIXED ledger ancestry rule

As of 2026-07-09, a `FIXED` ledger entry must point at landed history.
For superproject fixes, the recorded commit must satisfy:

```
git merge-base --is-ancestor <commit> trunk
```

For submodule fixes, the recorded submodule commit must be an ancestor of
the submodule commit pinned by `trunk`; once known, the ledger entry
should also name the superproject pin. Branch-only work is a candidate or
branch fix, not a trunk `FIXED` entry.

Ancestry audit scope: `.docs/audits/*.md` plus `noted_items.md`, checking
entries that named a commit-like hash against local `trunk` at
`163cf0c5`.

- Superproject FIXED entries verified as ancestors of `trunk`:
  `06e94f3c`, `c04476c2`, `531d19dd`, `ba67c10f`, `7a05e240`,
  `567ed0ee`, `91420e2f`.
- Submodule FIXED entries verified through the `trunk` submodule pin:
  afs-ld `b46543a` and `44ba4af` are both ancestors of the afs-ld
  commit `11831ef` pinned by superproject `trunk`.
- Stale FIXED entries that name commits present only on
  `origin/x12-campaign-x86`, not on `trunk`: `af214426`, `b3819f9a`,
  `5610f607`, `53c78f32`, `82c947e5`, `b1f0b796`, `3f177c24`,
  `9034c488`, `8de4188f`, `975fe776`. The mixed source/object driver
  fix from `82c947e5` was cherry-picked onto this remediation branch as
  `974f281f`; it is not trunk-FIXED until this branch lands.
- Unresolved short hashes referenced by FIXED entries but absent from the
  current superproject object database and the checked submodules:
  `36a51ad`, `b113ba3`, `9b50a7e`, `99cdc2d`, `267c8ce`, `cd6bbe9`,
  `3e15d80`. Resolve these to landed commits or downgrade the entries
  before relying on them as trunk-FIXED.
- FIXED entries that do not record a commit hash were not machine-checkable
  by the ancestry rule; add landed commits or pin commits to those entries
  when they are touched.

---

## test-drive (FreeBSD x86_64)

Status: **green** (2026-06-15).

- armfortas: build PASS (~250ms), test PASS (~570ms)
- gfortran 14.2.0: build PASS (~250ms), test PASS (~490ms)
- Differential: no step-outcome differences.

Build (both compilers, identical command):
```
{fc} -c src/testdrive_version.f90 && {fc} -c src/testdrive.F90
```
Test (compile library + suites + driver, link, run):
```
{fc} -c src/testdrive_version.f90 && {fc} -c src/testdrive.F90 \
  && {fc} -c test/test_check.F90 && {fc} -c test/test_select.F90 \
  && {fc} -c test/main.f90 && {fc} *.o -o tester && ./tester
```

The full test-drive suite (check + select) passes under armfortas,
including procedure-pointer test registration, derived-type test
suites, allocatable-string handling, and expected-fail accounting. No
compiler bugs surfaced on this rung.

## toml-f (FreeBSD x86_64)

**GREEN (2026-06-15): 215 PASSED, 21 EXPECTED FAIL, 0 real FAIL —
matches gfortran exactly** (gfortran: 215/215, same 21 should_fail). All
35 library sources compile; test-drive + lib + unit suite link and the
`tester` runs clean. Three real compiler bugs were found and fixed along
the way (#1 intent(out) polymorphic-allocatable segfault, #2
char-pointer-result dropping a class actual's descriptor, #3 scalar
derived value with descriptor-sized storage dropped its allocatable
content when passed by value). The 21 EXPECTED FAIL are toml-f's own
`should_fail=.true.` tests (verified in source + gfortran).

Build/run reproducer: `~/afs-scratch/build_tomlf.sh` (iterative compile
to resolve module order; path-unique object names — toml-f has both
`type/table.f90` and `build/table.f90`, basename collisions clobber).

Historical note — the original failure was **library compiles, test
binary SEGFAULTs on test 1** (bug #1). Kept below for the trail.

### Divergence: `class(_), allocatable, intent(out)` dummy crashes on entry (bucket: pure semantics/codegen)

Backtrace from the toml-f tester:
```
memset  <-  new_keyval (intent(out) init)  <-  add_keyval_to_array
        <-  set_elem_value_float_sp  <-  array_real_sp (test 1)
```
Reduced to a 12-line standalone repro:
```fortran
module m
  type :: base
    integer :: tag = 0
  end type
contains
  subroutine make(self)
    class(base), allocatable, intent(out) :: self   ! crash on entry
  end subroutine
end module
program p
  use m
  class(base), allocatable :: v
  call make(v)
  print *, 'ok'
end program
```
Bisected trigger: the `class(_), allocatable, intent(out)` dummy itself.
- empty body still crashes → it is the intent(out) ENTRY reset, not the
  allocate;
- `intent(inout)` is fine → intent(out) is the trigger;
- `type(_)` (non-polymorphic) allocatable intent(out) is fine → it is
  CLASS-specific.
armfortas mishandles the intent(out) descriptor reset (a memset) for a
polymorphic allocatable dummy. This pattern is pervasive in toml-f and
stdlib, so the fix likely unblocks a large fraction of both. Needs a
compiler fix + a `test_programs` regression fixture (not added yet —
campaign rule: the fixture lands with the fix).

**Bug #1 FIXED** (commit af21442): `intent(out)` on a polymorphic
allocatable dummy no longer crashes. toml-f went from crash-on-test-1 to
**211/215 pass** (gfortran: 215/215). The 21 EXPECTED FAIL are toml-f's
OWN `should_fail=.true.` tests (verified: 21 in source + gfortran shows
the same 21). The 4 remaining FAILs (array-string, table-array,
table-dateime, table-string) PASS on gfortran → genuine armfortas bugs,
not deferrals.

### Bug #2 (in progress): char-pointer function result drops a class actual's descriptor (bucket: pure semantics/codegen, ABI)

`array-string`/`table-string` fail with "expected 'aaa' but got ''".
Root cause reduced to ~15 lines: a function returning
`character(:), pointer` that takes a `class(_), intent(in), target`
dummy and does `select type(val); type is(sv); ptr => val%raw`. The
`select type` falls to `class default` because the CALLER passes the
class actual as a scalar data pointer, not its 384-byte descriptor:
```
%82 = load %0        ; v's data pointer (offset 0)
%86 = alloca ptr; store %82, %86
call cast(%76 /*hidden char result*/, %86 /*should be the descriptor*/)
```
An integer-pointer result through the identical function/select-type/
polymorphic-dummy path works (passes the real descriptor) — so it is
specific to the deferred-length char-pointer-result call path not
marking the class actual as wanting a descriptor (`wants_descriptor`
false at the string-call arg assembly). Mirrors toml-f
`cast_string`/`get_value_string`. Repro saved conceptually here; the
fix is caller-side arg lowering for char-result calls + a
`test_programs` regression fixture.

**Bug #2 FIXED** (commit b3819f9): scalar char-pointer-target pointer
assignment now threads descriptor_params, so `p => f(class_actual)`
passes the actual's descriptor. toml-f **211 -> 212** (array-string now
passes). Regression: `test_programs/x12_char_ptr_result_class_arg.f90`.

### Bug #3 (FIXED, commit 5610f60): scalar derived value with descriptor-sized storage dropped its allocatable content when passed by value

**FIXED — toml-f now 215/215 (gfortran 215/215), 0 real FAIL, 21 EXPECTED
FAIL (toml-f's own should_fail tests).** table-array, table-dateime,
table-string all pass.

Root cause (the earlier "table keyval / heap corruption / generic
resolution" theories were all wrong — see the superseded notes below):
a scalar derived-type VALUE whose storage is descriptor-sized — its only
component is an allocatable array — lowers to the SAME `Ptr<[i8;384]>` IR
type as an array DESCRIPTOR. toml-f's `toml_path` is exactly this:
`type(toml_key), allocatable :: path(:)` and nothing else, so
`sizeof(toml_path) == sizeof(array descriptor) == 384`.

In `lower_arg_by_ref_full` (src/ir/lower/core.rs ~45337, the
"evaluate-and-store-to-temp" fallthrough), a `Ptr<[i8;384]>` actual had
its first 8 bytes loaded as a `base_addr` — correct for an array
section / array binop / array-result function feeding an assumed-size or
explicit-shape dummy, but WRONG for a scalar derived value, which must be
passed by address. So `set_value(t, toml_path("a","b"), val)` passed
garbage; `walk_path` saw `path%path` unallocated and returned a fatal
stat (-1). The value only loses content when the constructor result is
passed DIRECTLY as the actual (a temp); assigning to a named var first
worked, because the named-var path takes its address via gep.

Fix: discriminate on the actual's declared RANK
(`actual_expr_rank`). Rank 0 = scalar → pass by address (no extraction);
rank >= 1 or unknown → keep the descriptor base_addr extraction (preserves
the stdlib gesv `a(lda,*)` path). This covers both the direct function
result (`mk(...)`) and the generic constructor that shares the derived
type's name (`toml_path(...)` → rank 0 via the structure-constructor arm).
Regression: `test_programs/x12_derived_ctor_temp_alloc_comp.f90` (named /
function-temp / generic-constructor-temp, all three asserted).

x86 regression after the fix: lib 1289/0, e2e 120/0 (all opt levels),
clippy clean.

macOS arm64 (nomad) verification — DONE (isolated /private/tmp worktree
off origin/x12-campaign-x86, own target dir, codex's main checkout
untouched):
- toml-f: 215 PASSED / 21 EXPECTED FAIL / 0 FAILED, exit 0 — identical to
  x86_64 and to gfortran. table-array/dateime/string all pass.
- x12 regression fixtures direct-run on arm64: all 3 green
  (x12_class_alloc_intent_out, x12_char_ptr_result_class_arg,
  x12_derived_ctor_temp_alloc_comp).
- Full `run_programs` e2e on nomad was inconclusive (stalled at 0% CPU
  under codex's concurrent suite — resource contention, not a failure);
  killed it rather than keep contending. The authoritative macOS gate is
  CI on macos-latest (cargo test -p armfortas --tests --release), which
  runs on the PR. The fix is platform-neutral IR lowering (pre-codegen);
  arm64 toml-f exercises the exact fixed path end-to-end.

#### Superseded diagnosis notes (kept for the record)

After bugs #1+#2, toml-f is **212/215** (gfortran 215/215). The 3
remaining FAILs are all the table value path:
- `table-string`: "expected 'value' but got ''"
- `table-dateime`: "expected '2019-12-17 18:26:59' but got ''"
- `table-array`: "Array was not created"
`table-real-sp` (simple real value in a table) PASSES, and array-of-
strings passes (bug #2). So the table keyval-pointer lookup
(`get_key_keyval`) works for simple values; the loss is specific to
values with allocatable/derived content (string=alloc char,
datetime=derived, array=nested) stored/retrieved through a table keyval
(set_child_value_string -> get_value(table,key,ptr) -> keyval set/get).
ROOT CAUSE (corrected — earlier "generic resolution" theory was WRONG;
IR disproved it): generic resolution is correct. Emitted IR for
`get_child_value_string` shows it correctly calls `get_child_keyval`
then `keyval get_value_string` (no recursion, no mis-resolution). The
earlier instrumentation that suggested mis-resolution was unreliable
(statements-before-declarations behave inconsistently in armfortas — a
separate minor issue worth noting).

The real symptom (reliably instrumented in the test itself):
`get_value(table,"string",val)` returns `val` **correct** — allocated,
`len=5`, content "value", first byte 'v' (verified by print AND by a
single-arg `character(len=*)` probe in the same scope). But when `val`
is then passed to test-drive's `check(error, val, "value")`, inside
`check_string` `actual` has `len=5` but **zeroed data** (the `/=`
compares true and `//actual//` yields ''). So `val`'s data buffer is
zeroed between the local-correct read and the `check` call.
- Not generic resolution (IR), not the `should_fail` accounting.
- Not a plain use-after-free: `MALLOC_CONF=junk:true` still shows zeros,
  not 0x5a (freed) — so it's an explicit zeroing / wrong-data-pointer,
  not read-after-free.
- Heap-layout-dependent: fails in the full tester (even `./tester build
  table-string` alone), but EVERY standalone reduction passes —
  single-module and cross-module generic unions, abstract-extended
  keyval, char-ptr/derived-ptr casts, `intent(out) error` + deferred
  `val` + literal vs function 3rd arg. All green standalone.
- Table-path specific: array-of-strings (list storage) passes; the table
  uses the ordered_map (structure/map.f90, ordered_map.f90). The 3
  failures (table-string/dateime/array) all read a value back through the
  table keyval.

Most likely a stray memset / wrong hidden-data-pointer that zeroes
`val`'s heap data, surfacing only under the full binary's heap layout
(probably while passing `val` as the 2nd actual to a generic `check`
after the `intent(out)`-allocatable `error` 1st actual — adjacent to
bug #1's intent(out) memset family). NEEDS a memory sanitizer to pin
(no valgrind/ASan on this FreeBSD box; armfortas has no -fsanitize).
Next session: get a sanitizer path (build the runtime/tester with an
asan toolchain, or bisect the ordered_map store/grow + the
check-call argument lowering). Distinct from #1/#2.

Remaining for green: reduce + fix #3 (target 215/215), then verify on
macOS arm64 (nomad). x86 regression after #1+#2: lib 1289, e2e 120,
clippy clean — both fixes are platform-neutral.

### Divergence: multi-file `-c` object placement (bucket: pure codegen / driver)

`armfortas -c a.f90 b/c.f90` writes each object **next to its source**
(`b/c.o`), but single-file `armfortas -c b/c.f90` and gfortran both
write `c.o` to the CWD. The objects are all produced, but the placement
difference breaks a naive `{fc} *.o -o bin` link line (armfortas's
objects aren't in CWD). Repro is trivial; classify as a driver
object-path quirk and decide whether to match the CWD convention or
require explicit `-o`. Not yet reduced to a standalone regression test.
Tracked here; does not block the library-compile result.

## stdlib (FreeBSD x86_64)

Pending. Large, fypp-preprocessed (needs python3 + fypp on the host),
submodule-heavy. Gated on l07 (submodules), l08 (CLASS vtable
dispatch), l09 (IEEE modules) — all now in trunk. Target: every module
compiles, native test pass rate ≥95%, every failure reduced +
classified.

## fpm (FreeBSD x86_64) — COMPLETE 2026-07-04

Self-hosting rung finished, beyond the original bar. Ladder, verified
on dorado (FreeBSD x86_64) AND hasu (NixOS glibc x86_64):

- stage 0: armfortas compiles the amalgamated fpm-0.13.0.F90 (53k
  lines) to a working 17MB binary. new/build/run/test all work.
- stage 1: that fpm drives ARMFORTAS as the backend compiler
  (`--compiler armfortas`): compile, -module dirs, .amod flow, ar
  archive, link — single + multi-module projects. The driver already
  spoke fpm's dialect (-c/-o/-I/-J/-module, .o inputs, -cpp no-op);
  no driver changes needed. fpm labels artifacts armfortas_<hash>.
- stage 2 (fpmception): fpm + armfortas build fpm ITSELF; the stage-2
  binary fully works.
- stage 3 fixed point: stage-2's fpm rebuilds fpm BYTE-IDENTICAL to
  stage 2 (cmp) — determinism held in anger.
- dependencies: a consumer project with toml-f as a PATH dependency
  builds and runs fully through armfortas (fpm resolves the dep,
  compiles all of toml-f, archives, links; the app parses TOML at
  runtime). GIT dependencies work too (cloned toml-f v0.4.2 from
  GitHub by tag, built, ran) — network fetch, tag checkout, full
  graph.

Compiler bugs found and fixed by this rung (PRs #85-#91), nearly all
one family — same-name procedure resolution across the 53k-line
amalgamation (fpm has many next/run/get/resize/to_string/join_path):
descriptor/optional/char-len mask caches keyed by mangled name (#86);
subroutine vtable dispatch never synthesizes a hidden result (#87);
bare call + function resolution through the caller's scope, not a
global source-order scan (#88); unit-wide internal_funcs no longer
hijacks generic calls, keyword args reordered against the RESOLVED
callee (#89); hidden-char-result calls resolve caller-aware — all 164
join_path calls had bound M_CLI2's instead of fpm_filesystem's,
mangling "../dep" paths (#90); same_unit_func_ref rebinds by host
association ONLY — the global last-match fallback let unrelated
internal subprograms hijack resolved calls, corrupting tomlf's node
arrays (toml_key strides over toml_node storage) and welding 2-arg
call vectors onto 3-param callees in diagnostic rendering (#91).
Plus runtime/I-O fidelity from the same rung: INQUIRE POS= for stream
units (the file-slurp idiom; was silently dropped), and PRINT honors
its format when items are procedure calls (the FMT_CTX stack made the
old list-directed fallback obsolete; E-notation leak seen live in
project output).

Upstream-fpm quirks classified by differential against a
gfortran-built fpm (identical behavior both builds, compiler
exonerated): app-only .F90 discovery; "OS Type: Linux" on FreeBSD
(/etc/os-release probe); absolute-path deps mangled ('./' prefixing).

Debug methods that carried the rung: truss/syscall diff of the stat()
paths each binary attempts; lldb with conditional breakpoints and
register/memory forensics at crash sites (a fault ADDRESS that equals
an enum value, an object pointer aimed into raw manifest text);
ddmin over manifest lines with string-shrink sensitivity probes;
env-gated compiler probes (entry/exit correlation in the call emitter
+ Backtrace::force_capture in FuncBuilder::call under
CARGO_PROFILE_RELEASE_DEBUG=true) — that correlation exposed the
resolved-name vs emitted-symbol mismatch at the heart of #91.

## fortsh (FreeBSD x86_64)

In progress (2026-06-15). Flagship: ~63k LOC F2018 POSIX shell, 55
Fortran sources + 4 C interop files, `make` build (gfortran on FreeBSD).

Build harness: FC wrapper `~/afs-scratch/afs-fc.sh` (drops gfortran-only
flags -fall-intrinsics/-fPIC, forwards the rest to armfortas release).
`gmake FC=<wrapper> CC=cc -j1` (serial: the Makefile's per-file deps are
written for gfortran's .mod timestamps and race under -j with armfortas's
.amod model). Reference for classification: a gfortran build of the same
tree (`~/afs-scratch/fortsh-gf`); armfortas binary saved at
`~/afs-scratch/fortsh-afs`.

### Build + run: GREEN
- All 55 Fortran sources compile under armfortas (iterative sweep 55/55).
- Links with the 4 cc-built C objects into a 33MB ELF; `Fortsh built
  successfully!`. Runs real shells: echo, fork/exec (`echo ... | wc -w`),
  `$((x+y))` arithmetic, pipelines, variables — all correct.

### Test suites
- **POSIX**: `run_all_tests.sh --posix-only` → **3776 passed / 0 failed /
  7 skipped**, deterministic over 3 runs (matches gfortran). The 7 skips
  are unconditional `skip` calls in posix_compliance_jobcontrol.sh
  (interactive job specs %1/%%, fg/bg suspended, disown) — framework
  skips, identical under any compiler, NOT armfortas deferrals. (One
  earlier flake on coverage 321.1 `glob [:alpha:]` was the test's own
  race on a hardcoded `/tmp/gc`; green in isolation and on re-runs.)
- **integration**: `integration/run_integration_tests.sh` → **479 passed
  / 0 failed / 3 skipped**, 100%.
- **builtins**: `run_builtin_tests.sh` → **838 passed / 2 failed / 4
  skipped** (was 833/7 before bug A fix). The 2 remaining are the
  recursion stress tests (bug B). Original 7 failures (all in
  test_stress.sh, all GENUINE armfortas bugs vs gfortran):
  - bug A: large arg lists truncate (`echo $(seq 1 N) | wc -w` → afs
    308/520/208 for N=500/1000/400, gfortran/bash N). 5 of 7 failures.
    DETERMINISTIC (308/520/208 stable across runs → codegen bug, not
    corruption). Narrowed: NOT general word-splitting — `for w in
    $(seq 1 500)` counts 500 correctly and `%0*d` 2000-char single-string
    capture is correct; the big truncation is the `echo`-builtin output /
    pipe-write path with many args (echo of ~500 args → ~308 words through
    a pipe). `set -- $(seq 1 500); echo $#` → 497 (a separate small
    off-by-few in word-split). PINNED: `echo $(seq 1 500)` piped to the
    *system* wc emits only 308 words / 1206 bytes (bash 1892, gfortran-
    fortsh 1892) — so the loss is in fortsh's ECHO builtin output assembly
    (a ~1200-byte buffer), not the pipe and not word-splitting. Same source
    under gfortran is correct, so armfortas miscompiles a sized buffer /
    length calc in the echo arg-join path. FURTHER PINNED: the cap is on
    TOKEN COUNT (~308), not bytes — `echo $(seq 1000 1500)` (4-digit
    tokens) emits 309 words/1545 bytes, `echo $(seq 1 500)` 308 words/1206
    bytes (same ~308 count, different bytes). The COMMAND's token array for
    builtins (cmd%tokens / cmd%num_tokens, builtins.f90 builtin_echo) caps
    at ~308; positional params are unaffected (`c(){ echo $#; }; c
    $(seq 1 500)` → 500). The builtin output WRITE path is fine (single
    5000-char arg echoes 5001 bytes). RULED OUT: generic allocatable-array
    growth `a = [a, x]` to 500 — works on armfortas (repro passed). So the
    fortsh command-token array uses a different grow/copy construct
    (manual capacity doubling, move_alloc, or a derived-type component
    array realloc) that armfortas miscompiles past ~308.
    TRACED (expansion.f90): `expand_word` (4426) fills a CALLER-sized
    `expanded_words(:)` array; `word_split` (called at 4464) caps at
    `size(expanded_words)`. Same fortsh code gives 500 on gfortran, 308 on
    armfortas → armfortas miscompiles the caller's word/field-COUNT
    estimate that sizes expanded_words (computes ~308 vs true 500),
    under-sizing the array so word_split truncates. The function-arg path
    uses different (dynamic) storage → unaffected (500). Next: find
    expand_word's caller in the command-exec path + the word-count loop it
    uses to size expanded_words; reduce that counting loop over a long
    string to a minimal armfortas repro. (Generic char-array growth and the
    builtin write path are both confirmed correct, so the bug is the
    count/size computation, not realloc or I/O.)
  - recursive shell functions (bug B): `f(){...f $(($1-1));}; f 75` → afs
    SIGSEGV (rc 139), gfortran/bash 0. DIAGNOSED, deferred (needs a larger
    codegen change — see "Bug B" below). 2 of 7 failures.
- **unit/bench** (Fortran test programs armfortas compiles directly):
  test-lexer ✓, test-memory-pool ✓, test-executor ✓; test-suggestions and
  test-highlight previously SEGV'd (bugs C/D) — now **FIXED** (30/0 and
  227/0, matching gfortran) by commit 53c78f3 (host-assoc closure vars
  forwarded through sibling internal calls). Root: armfortas's
  host-association closure ABI computed a contained proc's hidden host-ref
  params from direct refs + nested children only, missing names a proc must
  forward to a SIBLING it calls -> garbage pointer -> SIGSEGV. The
  test harnesses (`test_x` calls sibling `assert_*` that bump host
  counters) hit this on the first assert. Repro:
  test_programs/x12_internal_peer_call_host_assoc.f90. lib 1289/0, e2e
  120/0, clippy clean after the fix.
  - Surfaced a driver gap first: armfortas rejected mixed
    `fc test.f90 build/foo.o -o test`. FIXED (commit 82c947e): compile_multi
    now partitions sources from prebuilt artifacts and links both in
    command order. Regression: tests/multifile.rs
    mixed_source_and_object_in_one_invocation.
- **interactive PTY** (pexpect, `FORTSH` env var, NOT FORTSH_BIN; ~20
  files, slow — failing tests each burn a pexpect timeout). Clean full
  armfortas run: **107 passed / 11 failed** (616s). IMPORTANT: a partial
  run under concurrent load showed many more failures — PTY is
  timing/load-FLAKY here, so failure lists must be taken from an
  unloaded full run and re-checked in isolation before classifying.
  The 7 readline-editing files (37 tests) all pass on gfortran-fortsh
  (37/0); under armfortas they pass in the clean run too (so the earlier
  "37 fail" was load-induced flake — corrected). Two stable AssertionError
  failures seen: test_command_richness::test_git_subcommand_prefix_filters
  and test_menu_descriptions::test_git_subcommand_menu_shows_help (git
  subcommand completion descriptions). Bug E = the stable PTY failures —
  now FULLY CLASSIFIED + FIXED (see "Open armfortas bugs" E below): a clean
  unloaded run was 12 failed / 106 passed; 10 were armfortas-specific (one
  root cause: fixed-len allocatable char scalar passed as len=* → length 0),
  2 fail on gfortran too. After commit 975fe77: PTY 116/2, matching
  gfortran (the 2 residual are the gfortran-shared git-subcommand tests).

### Open armfortas bugs to fix for green
A. FIXED (commit b1f0b79) — was: command/builtin arg expansion truncates
   at ~308 tokens. ROOT (not capture, not field_split — both fine in
   isolation): build_host_ref_params typed a host-associated var's element
   via lower_type_spec, which returns Ptr<i8> (8 bytes) for ANY derived
   type. So indexing a host-associated derived ARRAY from a contained proc
   strided by 8 instead of the struct size -> element k>1 wrong / SIGSEGV
   (k=1 at offset 0 was fine; only bit derived types > 8 bytes, e.g. a type
   with an allocatable component = 32-byte string descriptor). fortsh hit
   it in pipeline_helpers grow_temp_arrays growing a host `string_t` token
   array past its initial 256 cap during word splitting. Fix: resolve the
   host-ref element type through the type-layout registry
   (dummy_local_ir_type), like a normal dummy. `echo $(seq 1 500)` -> 500
   words; stress 197->202, builtins 833/7 -> 838/2. lib 1289/0, e2e 120/0,
   clippy clean. Repro: test_programs/x12_host_derived_array_stride.f90.
   (Minimal: a host `type(t),allocatable::a(:)` with `t` embedding an
   allocatable component, indexed at k>1 from a contained sub.)
   Superseded note — was: command-substitution output capture
   (truncation/empty on large output)
B. deep recursive shell-function execution — FIXED
   (commits 3f177c2, 9034c48, 8de4188).
   `f(){...f $(($1-1));}; f 75` SIGSEGV'd at recursion depth ~62 (<=60 ok,
   >=65 rc 139); gfortran/bash reach 100+. ROOT (the earlier diagnosis was
   WRONG — it blamed block-local alloca hoisting, which is real but minor
   ~17KB): the naive spill-everything register allocator at -O0 gives every
   SSA value its own stack slot. fortsh builds at -O0, so the huge
   non-recursive `execute_simple_command` got a 134KB frame (gfortran:
   10.6KB / 0x2a98) and sits on the stack at every recursion level ->
   overflow at ~62. Verified by objdump prologue `sub rsp` of the hot
   functions vs the gfortran build (~/afs-scratch/fortsh-gf): naive 33
   pages, linear-scan 5 pages for the same function. gfortran does real
   register allocation at -O0; armfortas did not. FIX: linear-scan is now
   the allocator at EVERY opt level on both backends (was O1+); naive
   survives only behind ARMFORTAS_USE_NAIVE_REGALLOC. Before flipping,
   hardened the 3 audited linear-scan fragilities (class-divergence +
   scratch asserts: silent miscompile -> panic). The flip is x86-ONLY:
   arm64 -O0 stays on naive so macOS codegen is byte-identical to before
   (the arm64/mod.rs diff vs parent is comment-only). RESULT (x86/dorado):
   fortsh recursion depth 200 rc 0, stress 204/0 (was 202/2), builtins
   840/0 (was 838/2), run_programs 120/0, lib 1290/0, clippy clean.
   Guard: codegen::tests::x86_linear_scan_at_every_opt_level (a unit test
   asserting the policy — re-adding an opt-level gate resurrects bug B).
   An earlier runtime fixture (x12_deep_recursion_frame.f90) was DROPPED:
   it overflowed on macOS arm64 -O0 because arm64 keeps naive there, and a
   runtime-overflow test can't be both allocator-distinguishing and
   portable to an arm64-naive-O0 target. fortsh's own suite is the runtime
   validation.
   macOS triage (deliverable note): the initial nomad run that looked red
   (7 failures) was LOAD FLAKINESS — a single program
   (defined_assignment_derived_operator_result) timing out under a 43-min
   crush, counted across all 6 batch levels + 1 per-fixture test. Lighter
   re-runs: trunk -O0 batch clean, x12 -O0 batch clean except the (now
   removed) bug B fixture. No pre-existing macOS regressions. arm64 -O0
   stays on naive: we flipped it to linear and validated on quiet nomad —
   it's a REGRESSION on arm64 (recursive function overflows at depth ~4000
   under linear vs ~5-6000 under naive; arm64 callee-save/split-bridge
   overhead exceeds the spill win on small functions, opposite of x86).
   lib 1291/0 under arm64-linear@O0 (mostly correct, just bigger frames),
   BUT the full run_programs gate also showed 2 -O0 failures absent under
   naive (do_loop_vectorize_fma, do_loop_vectorize_minmax_binary) — real
   miscompile or flake, unclassified. Reverted; bug B fix is x86-only.
   See noted_items.md.
C. suggestions module segfault — FIXED (commit 53c78f3)
D. syntax_highlight module segfault — FIXED (commit 53c78f3)
E. stable PTY failures — CLASSIFIED + FIXED (commit 975fe77).
   Clean unloaded run: 12 failed / 106 passed (NOT ~11 flaky as feared —
   stable). Classified each against the gfortran build (~/afs-scratch/fortsh-gf,
   same source): 10 were armfortas-specific (autosuggestion ×7,
   command_completion ×2, menu_descriptions git-help ×1), 2 fail on
   gfortran too (test_git_subcommand_menu, _prefix_filters — a fortsh/env
   issue, not ours). All 10 armfortas failures had ONE root cause: a
   character(len=N), allocatable SCALAR passed to a character(len=*) dummy
   passed length 0 (local_char_runtime_len's CharKind::None arm returned
   None for fixed-len allocatable char scalars). readline's current_input
   is exactly that, so compute_history_suggestion saw an empty prefix and
   no completion/suggestion matched. Fix: pass the declared constant via
   local_fixed_char_allocatable_scalar_len. After fix: PTY 116/2 (the 2
   are the gfortran-shared git tests) — armfortas now MATCHES gfortran on
   the PTY suite. Fixture: test_programs/x12_alloc_char_scalar_assumed_len.f90.
   lib 1290/0, run_programs 120/0, clippy clean.

## afs-as x86_64/ELF (x13 + x14) — COMPLETE 2026-07-04

The bespoke assembler now owns the ELF pipeline end to end. No system
`as` in the default path on FreeBSD or Linux.

x13 (ELF writer): afs-as/src/elf.rs — ELF64 relocatable model, writer,
reader, validate. Record-level byte fidelity against gas 2.44 (dorado)
and 2.46 (hasu); EI_OSABI pinned FreeBSD=9 / Linux=0; RELA-only,
locals-first symtab, gas NOBITS conventions. Merged afs-as #8's
predecessor (elf-writer, 570fdf7), pin PR #92.

x14 (x86 encoder + assembler): AT&T-subset parser (self-contained),
table-driven encoder (REX/ModRM/SIB, integer + SSE families, movq
imm64 widening, width-reinterpreted immediates), two-pass assembler
with rel8/rel32 relaxation fixed-point, gas local-symbol model
(same-section PC-rel resolved at assembly, cross-section folded to
STT_SECTION), .size dot markers, gas NOP fill. afs-as PRs #8 + #9.

Referees, all green on dorado (gas 2.44) and hasu (gas 2.46):
- per-instruction differential: 175 cases byte- + reloc-identical
- whole-file: 6 backend fixtures + 8 relaxation boundary cases
- whole-corpus (root crate): every test_programs at -O0/-O2/-O3,
  1623 objects, section bytes + relocs + symbols equal
- seeded fuzz: 96 program-shaped seeds vs gas; 512 garbage seeds
  error-not-panic; 128 stress seeds roundtrip write/parse/write
- NOP fill compared modulo binutils split order (2.44 longest-first,
  2.46 remainder-first) — found on hasu, 614 padding-only diffs

Fuzzer catches before any user did: movq $imm64 must widen to movabs
form; orw $65535 must sign-reinterpret to the 83 imm8 form.

Driver flip: in-process afs_as::x86::assemble default for ELF targets;
AFS_AS_PATH subprocess contract kept (afs-as grew `--64` CLI);
AFS_AS=0 falls back to system as. Cross-arch `-c` now works from any
host (in-process pipeline); cross-linking still errors with guidance.
run_programs 120/120 on all three routes (in-process, AFS_AS_PATH,
AFS_AS=0); determinism_sweep green; fpm bootstrap ladder
(stage0 -> stage3 byte-identical fixed point) green with the
in-process assembler.
