# Sprint 31 Brutal Audit

Date: 2026-04-13
Branch: `module-system` (sprint 31 commits on `trunk`)
Compiler build: release, clean cargo build successful.

## Executive summary

Sprint 31 demonstrably fixed the 19 issues it claimed to. The library-unit and
integration test suites are entirely green (981 unit + ~3000 other tests, 11
`#[ignore]` for i128). The trouble is everything outside the tested surface:
running fortsh through the compiler reveals that **only 8 of fortsh's 55 source
files compile at -O0 with -c** (14.5%). Our audit uncovered **7 net-new CRITICAL
bugs** that the current test bench never exercises (nested host association,
keyword argument reordering, character parameter/initializer lowering, derived-
type function-result assignment, explicit-shape bound with dummy argument,
assumed-shape size() in function results, F2003 IMPORT statement parsing), plus
a larger cluster of MAJOR parse/sema gaps (procedure-pointer targets from dummy
args, pointer-component substring `=>`, typed ALLOCATE on derived-type
components, allocatable-component ALLOCATE, logical-operand coercion, mixed-kind
bitwise coercion, BLOCK-scope finalization). Sprint 31 was a competent round of
plumbing; sprint 32 should be dominated by these newly-revealed issues before
anyone attempts the full fortsh build that Sprint 33 targets.

## Baseline state

- `cargo build --release --bin armfortas` — OK.
- `cargo test --workspace --release` — all crates green. Totals observed:
  - 981 armfortas unit tests, 1062 afs-tests lib, 411 + 375 two largest
    integration suites, ~26 other integration suites all PASS.
  - 11 `#[ignore]` tests (i128 codegen — matches `noted_issues.md` #475).
- Recent commits skimmed (`git log --oneline -25`): 19 sprint-31 commits map
  1:1 to tasks #455–#480 except #475 (still deferred, correctly).
- `.docs/noted_issues.md` last updated on this branch; `(B)` items tied to
  sprint 30 (Module System) are now visibly partially addressed (cross-TU
  module globals work; see §X.4 "WORKS"), but the header summary was not
  updated to reflect sprint-31 reality.

## fortsh compilation headline

Methodology: enumerate `/Users/matthewwolffe/Documents/GithubOrgs/FortranGoingOnForty/fortsh/src/*.f90`,
build dependency graph from USE statements (intrinsics excluded), topological-sort, compile each
with `armfortas -c -O0 -I <workdir>` writing .amod/.o to the same dir.

Result: **8 pass, 47 fail, cascading**. Direct-error failures (not cascaded from missing .amod):

| File | Root cause |
|------|-----------|
| `common/memory_profiler.f90` | IR verify `logical op %48: operands must be Bool (i8 / bool)` |
| `common/performance.f90` | sema: `only allocatable or pointer variables can appear in ALLOCATE, but 'token_pools' is neither` — fails on `allocate(token_pools(id)%tokens(n))` |
| `common/string_pool.f90` | sema: `pointer assignment target 'ref' must have pointer attribute` — fails on `ref%data => pool_64(slot)(1:length)` |
| `io/suggestions.f90` | codegen emits illegal `sxtw w26, w27` (dest must be X-reg) |
| `common/memory_dashboard.f90` | depends on string_pool (cascade) |
| `execution/builtin_interface.f90` | IR: `coerce_to_type: unhandled coercion Ptr(Int(I8)) → Bool` |
| `execution/trap_dispatch.f90` | sema: `pointer assignment source 'proc' must have target or pointer attribute` — fails on `trap_evaluator => proc` where `proc` is a dummy procedure argument |
| `parsing/lexer.f90` | codegen emits illegal `sxtw Wd, Wn` in larger functions |
| `scripting/printf_builtin.f90` | IR verify `logical op %142: operands must be Bool (i8 / bool)` |
| `system/interface.f90` | IR verify `bitwise op %285: operand width mismatch i32 vs i64` |
| `system/signal_handling.f90` | IR verify: mixed bool/i8 on logical ops + pointer-stored-as-bool |
| `execution/coprocess.f90` | parse error: `import :: c_int, c_pid_t` (F2003 IMPORT stmt in interface body) |
| `execution/jobs.f90` | cascade (needs system_interface) |
| `io/syntax_highlight.f90` | cascade |
| `parsing/glob.f90` | cascade |
| `system/signals.f90` | parse error on IMPORT |
| `execution/better_errors.f90` | cascade |
| `io/fd_redirection.f90` | cascade |
| `parsing/grammar_parser.f90` | cascade |
| `scripting/aliases.f90` | cascade |
| `scripting/substitution.f90` | cascade |
| `scripting/variables.f90` | cascade |
| `io/heredoc.f90` | cascade |
| `scripting/advanced_test.f90` | cascade |
| `scripting/config.f90` | cascade |
| `scripting/directory_builtin.f90` | parse error on IMPORT inside interface |
| `scripting/expansion.f90` | cascade |
| `scripting/getopts_builtin.f90` | cascade |
| `scripting/prompt_formatting.f90` | cascade |
| `scripting/read_builtin.f90` | cascade |
| `parsing/parser.f90` | cascade |
| `scripting/test_builtin.f90` | cascade |
| `execution/pipeline_helpers.f90` | cascade |
| `scripting/completion.f90` | cascade |
| `scripting/control_flow.f90` | parse error on `got %` (late in file, 1176:10) — needs further reduction, dependencies blocked |
| `io/readline.f90` | cascade |
| `scripting/shell_options.f90` | cascade |
| `execution/executor.f90` | cascade |
| `execution/ast_executor.f90` | cascade |
| `execution/command_capture_callback.f90` | cascade |
| `execution/eval_builtin.f90` | cascade |
| `scripting/command_builtin.f90` | parse error IMPORT |
| `execution/builtins.f90` | cascade |
| `fortsh.f90` | cascade |

Compile log sources are under `/tmp/audit31/fortsh/*.log`.

If we fix the ~10 distinct root-cause issues above, the `.amod` cascade should
unblock roughly 35 of the 47 failures mechanically. With the bitwise-kind and
procedure-pointer issues resolved, we expect fortsh to reach **≥85%** module
coverage at -O0.

## Cross-opt ABI matrix

We built `audit31_brutal_crossopt_{lib,main}.f90` and compiled each file
independently at every permutation of `{-O0,-O1,-O2,-O3,-Os,-Ofast}`. The
intent was to verify that the closure-passing ABI changes do not introduce
opt-level-dependent divergence. Findings (see §X.3 for reproducers):

1. All 36 permutations **crash at runtime** because `sum_arr(xs, n, r)` — an
   **explicit-shape dummy** whose bound `n` is itself a dummy argument — emits
   a bounds check of `[1, 1]` instead of `[1, n]`. The bug is not opt-level
   dependent; it is present at `-O0` and equally at every other level. So the
   matrix cannot yet distinguish ABI drift from the underlying codegen bug.
   → **See Finding 2.**
2. When I simplified the test harness to scalar/derived-type/character calls
   only, all 36 permutations produce byte-identical output, meaning the
   scalar-and-struct portion of the ABI IS stable across opt levels.
3. We did not test nested-contains or host-closure cross-opt because the
   closure-passing ABI by construction lives inside one compilation unit
   (`contains` blocks can't be separately compiled). Cross-opt is a no-op
   concern there; what matters is **inside** a TU at a single `-O`, which the
   existing tests already cover.

## Findings (severity-sorted)

Each finding has a reproducer under
`test_programs/audit31_brutal_*.f90`.

### 1. CRITICAL: Keyword arguments are IGNORED during dispatch
- **Reproducer:** `test_programs/audit31_brutal_keyword_args.f90`
- **Call:** `call sub(b=10, a=20)` where sub has `(a, b)`.
- **Expected:** `a=20, b=10`.
- **Actual:** `a=10, b=20`. Keywords parsed but not used to reorder actual args.
- **Impact:** Silent wrong-value bug. fortsh/variables.f90, readline,
  printf_builtin all use keyword args; miscompiled programs run with wrong data.
- **Suspect:** `src/sema/resolve.rs` or `src/ir/lower.rs` around call-site
  construction — need to permute actual args by keyword-match to dummy list.

### 2. CRITICAL: Explicit-shape dummy with dummy-arg bound loses upper bound
- **Reproducer:** `test_programs/audit31_brutal_explicit_shape_bounds.f90`
- **Pattern:** `subroutine s(xs, n, r); integer, intent(in) :: n, xs(n)`.
- **Expected:** sums `xs(1..n)` with `n=5`.
- **Actual:** `Bounds check failed: index 2 outside [1, 1]`.
- **IR diagnostic:** `rt_call @__afs_check_bounds(i, 1, 1)` — hardcoded `1`
  in place of `n` at the call site's bounds check. Literal bounds like
  `xs(5)` work. Module-level PARAMETER bounds work. Only dummy-arg bounds break.
- **Impact:** Basically every Fortran 77 / LAPACK / BLAS API signature.
- **Suspect:** `src/ir/lower.rs` array-descriptor emission for dummy args
  — the upper bound expression is evaluated in the wrong scope before the
  dummy is bound. Fix likely requires lowering `xs(n)` as "explicit-shape
  with deferred bound evaluation inside callee prologue".

### 3. CRITICAL: Character variable/parameter initializers silently lost
- **Reproducer:** `test_programs/audit31_brutal_char_init.f90`
- **Pattern:** `character(len=5) :: a = 'hello'` → `a` prints `'     '`.
- **Also:** `character(len=5), parameter :: b = 'world'` — same.
- **Expected:** `[hello]` / `[world]`.
- **Actual:** `[     ]`. Integer initializers work. Literal strings passed
  directly to PRINT also work. `len(b)` correctly returns 5 (length is
  stored, the content is not).
- **Impact:** Every module-level string constant, every default-valued
  string field, is blank at runtime. fortsh has ~200 such parameters.
- **Suspect:** `src/ir/lower.rs` `init_decls` pass — has an integer-init
  path for scalars, but the character path is missing a memcpy of the
  literal bytes to the alloca/global.

### 4. CRITICAL: Nested CONTAINS: inner's writes to host var are not propagated
- **Reproducer:** `test_programs/audit31_brutal_nested_host.f90`
- **Pattern:** program has `host_var`; program contains `outer`; `outer`
  contains `inner`. `inner` does `host_var = host_var * 2`.
- **Expected final value:** 20.
- **Actual:** 10. `inner` reads host_var correctly; its writes land on its
  own stack copy (or outer's), not the real program-scope slot.
- **Impact:** Sprint 31 fix for host association (task #456) only handles
  one level of nesting. Two-level is broken. fortsh has a handful of these
  (readline helpers, parser lookahead).
- **Suspect:** `src/ir/lower.rs` `walk_contained_host_refs` — does not
  recurse into a contained's own contained procedures to aggregate host
  refs up to the ultimate owner.

### 5. CRITICAL: Assigning a derived-type FUNCTION result crashes (SIGSEGV)
- **Reproducer:** `test_programs/audit31_brutal_derived_fn_assign.f90`
- **Pattern:** `c = add_t(a, b)` where `add_t` returns `type(t)`.
- **Actual:** SIGSEGV (ec=139) before any print. Accessing the result via
  component chain like `add_t(a,b)%x` doesn't crash but returns 0 — so both
  paths are broken.
- **Impact:** Task #476 ("Derived-type function result drops body assignments")
  was marked CRITICAL+completed in this sprint. It's not fully fixed: the
  body-assignment portion inside `add_t` apparently runs (we can see the
  code in IR), but the caller side still corrupts the slot we assign into.
- **Suspect:** Return-slot ABI for derived-type-by-value. Likely the callee
  writes into a slot pointed to by a "hidden sret" arg, but the caller's
  alloca for `c` isn't passed — instead garbage goes into a temp that is
  dereferenced for the store.

### 6. CRITICAL: Assumed-shape array in a FUNCTION returns `size() = 0`
- **Reproducer:** `test_programs/audit31_brutal_func_assumed_shape.f90`
- **Pattern:** `function f(xs) result(r); integer :: xs(:); r = size(xs)`.
- **Expected:** n (the caller's array size).
- **Actual:** 0. Existing test `realworld_assumed_shape_size.f90` uses a
  SUBROUTINE (works). Function form did not have coverage.
- **Impact:** Any numerical routine that returns a reduction over its
  array arg. Many common idioms.
- **Suspect:** `src/ir/lower.rs` descriptor-pass path for function
  parameters — the caller builds the descriptor but the callee's hidden
  `__descriptor_*` shadow arg isn't synthesized for function callees
  (only subroutine callees). Possibly specific to the fcall-result-alloca
  path colliding with the descriptor-arg path.

### 7. CRITICAL: F2003 `IMPORT` statement inside interface block: parse error
- **Reproducer:** inline in `audit31_brutal_import_stmt.f90` (new; see
  reproducer below).
- **Pattern:** `interface; function foo_c(x) bind(C) result(r); import ::
  c_int; integer(c_int), value :: x; integer(c_int) :: r; end function;
  end interface`
- **Expected:** parsed, `c_int` becomes visible inside the interface body.
- **Actual:** `parse error: expected expression, got ::` at the `::` of
  the IMPORT.
- **Impact:** Blocks ~7 fortsh files directly; every real ARM64 codebase
  using `iso_c_binding` with named C interfaces inside explicit INTERFACE
  blocks.
- **Suspect:** `src/parser/stmt.rs` — no recognizer for `IMPORT` statement.

### 8. MAJOR: Procedure pointer assignment from a dummy procedure argument
  rejected
- **Reproducer:** inline trap_dispatch reduction.
- **Pattern:**
  ```
  subroutine set_cb(proc)
    procedure(iface) :: proc
    callback => proc
  end subroutine
  ```
- **Actual:** `error: pointer assignment source 'proc' must have target or
  pointer attribute`.
- **Expected per F2003:** a dummy procedure is a valid target for a
  procedure-pointer assignment. Dummy procedures are implicitly addresses.
- **Suspect:** `src/sema/validate.rs` pointer-assignment checker missing
  "source is a (dummy) procedure" relaxation.

### 9. MAJOR: Pointer-component `=>` with substring RHS rejected
- **Reproducer:** string_pool excerpt.
- **Pattern:** `ref%data => pool_64(slot)(1:length)`.
- **Actual:** `error: pointer assignment target 'ref' must have pointer
  attribute`. The error cites the wrong entity (`ref` is NOT the target;
  `ref%data` is, and it IS a pointer).
- **Impact:** String-pool interning, any custom allocator using shared
  backing buffers.
- **Suspect:** sema pointer-assignment target resolver collapses `X%Y` to
  `X` and checks attributes on the base instead of the final component.

### 10. MAJOR: `allocate(derived%alloc_component(n))` rejected
- **Reproducer:** performance.f90 snippet.
- **Pattern:** `allocate(token_pools(i)%tokens(n))` where `token_pools` is
  a fixed-size array of derived type, `tokens` is an allocatable component.
- **Actual:** `error: only allocatable or pointer variables can appear in
  ALLOCATE, but 'token_pools' is neither`. Sema checks the base of the
  allocate expression; it should walk through component selection to the
  leaf allocatable.
- **Impact:** Any object-pool pattern. fortsh has four of them.

### 11. MAJOR: Typed ALLOCATE (`allocate(character(len=256) :: x%y(n))`)
  rejected
- **Reproducer:** reduction of control_flow.f90 line 1141.
- **Pattern:** `allocate(character(len=256) :: cmd%tokens(num_tokens))`.
- **Actual:** `only allocatable or pointer variables can appear in ALLOCATE,
  but 'cmd' is neither`.
- **Related to #10** — same base-vs-leaf confusion, compounded by F2003
  typed allocation syntax.

### 12. MAJOR: Codegen emits illegal `sxtw Wd, Wn`
- **Reproducer:** `test_programs/audit31_brutal_sxtw.f90` (did NOT repro a
  simple case — requires larger functions that exercise a specific path).
  The fortsh files `io/suggestions.f90` and `parsing/lexer.f90` both trip
  it. The IR shows `%N = int_extend %M : i32 signed : i32` — a sign-extend
  to i32, which is nonsense (SXTW is 32→64). Upstream sema/lowering is
  producing an `int_extend` with a target type that is smaller or equal to
  the source; isel translates this to SXTW without checking dest width.
  Also the IR `%M = call @last_word(...) : i8` (returning i8 instead of
  i32) reveals the trigger: when sema resolves a character variable used
  as a substring `s(i:i)` but mis-classifies it as a function call
  (returning character-scalar i8), the downstream "widen to i32" inserts a
  bogus i32-to-i32 sign-extend. Two bugs stacked: (a) substring-in-a-
  function-result misresolves as a call, (b) int_extend i32→i32 isel'd to
  SXTW.
- **Impact:** Compiler emits assembly the in-house assembler rejects.

### 13. MAJOR: Logical-operand coercion produces `i8 / bool` mismatch in IR
- **Files:** memory_profiler, printf_builtin, signal_handling.
- **Pattern:** sema inserts a Convert from a byte-typed value to Bool
  but the verifier catches it. Since sprint-31 task #472 relaxed the
  verifier's type-cache short-circuit, these previously silent stalls
  now surface as verify errors. The relaxation exposed them; the fix
  is to make `coerce_to_type` handle Ptr(i8) and i8 → Bool explicitly
  at the IR insertion site, not to re-suppress the verifier.
- **Impact:** Three files in fortsh; many other C-interop flavoured
  programs will hit this.

### 14. MAJOR: Mixed-kind bitwise ops (c_int and c_long operands)
  produce width mismatch
- **File:** system/interface.f90.
- **Pattern:** `iand(val_c_long, not(int(z'400', c_int)))` — right operand
  is i32, left is i64, no automatic widening.
- **Suspect:** `src/sema/resolve.rs` for `IandExpr`/elemental intrinsics
  — unify operand widths (promote to larger) before IR insertion.
- **Impact:** Cross-kind bit manipulation (termios, stat flags) hits this.

### 15. MAJOR: BLOCK-scope finalizer does not fire
- **Reproducer:** `test_programs/audit31_brutal_block_finalizer.f90`
- **Expected (F2018 §7.5.6):** FINAL subroutine invoked at END BLOCK.
- **Actual:** silently not fired. Program-scope finalizer DOES fire (we
  verified `test_programs/derived_type_final.f90`, which is scoped at
  program level).
- **Impact:** RAII-style cleanup in BLOCK constructs (locks, temp files).

### 16. MAJOR: `IMPLICIT integer (i-n)` inside BLOCK not honored
- **Reproducer:** inline in `audit31_brutal_implicit_block.f90`.
- **Pattern:** outer has `implicit none`; a BLOCK inside declares
  `implicit integer (i-n)`; using `n` inside the block yields "undeclared
  (IMPLICIT NONE is active)".
- **Impact:** F2008 feature. Gap, not critical.

### 17. MINOR: SUBMODULE parses but emits no body
- **Reproducer:** `/tmp/audit31/amod/submod.f90`.
- **Symptom:** Linker "Undefined symbols: _compute".
- **Per noted_issues.md:** submodule listed as "(D) fortsh has zero
  submodules" — so fortsh isn't blocked, but silent-parse-no-emit is
  worse than a parse error, since callers link against a ghost.

### 18. MINOR: Multi-line string with `&` continuation and `!` on next line fails
- **Reproducer:** `/tmp/audit31/sprint31probe/bang_amp_multi.f90`.
- **Diagnostic:** `lexer error: unterminated string literal`.
- **Sprint 31 #470 fixed the single-line case**; the continuation-line
  counterpart slipped through.

### 19. MINOR: Cross-TU operator(+) where the function result is derived-type
  crashes
- **Reproducer:** `/tmp/audit31/amod/op_lib.f90 + op_main.f90`.
- **This is Finding 5** surfaced via the operator-interface entry path.
  Same underlying bug (derived-type function result ABI), separate
  reproducer path.

## WORKS (explicit positives)

Worth recording to contrast:

- Module-level global initializer: integer & cross-TU — WORKS.
- Custom defined operator `(.add.)` — WORKS.
- `interface assignment(=)` cross-TU — WORKS.
- Generic dispatch on kind (double vs single) — WORKS (sprint 31 #458).
- Generic dispatch for SUBROUTINE — WORKS (sprint 31 #464).
- Generic interfaces across .amod — WORKS (sprint 31 #462).
- Generic interface via renamed USE — WORKS (sprint 31 #478).
- `&!` in string (single-line) — WORKS (sprint 31 #470).
- Implied-do `(/ (i, i=1,5) /)` — WORKS (sprint 31 #471).
- `dimag` intrinsic and complex POINTER init — WORKS (sprint 31 fix).
- Multiple procedure pointers on one line — WORKS (sprint 31 #473).
- 50-module-deep linear `use` chain — WORKS.
- Large arrays above 64KB stack threshold — WORKS.
- Deeply nested derived types — WORKS.
- Recursion at every opt level — WORKS.
- Host association one level (contains) — WORKS (sprint 31 #456).
- Kind-suffix `_dp` real literals — WORKS (sprint 31 #455).
- IPO const-arg specialization — CI covers it (sprint 31 #480).

## Test bench gap summary

Test suites counted (`grep -l` in `test_programs/`):
- `ERROR_EXPECTED`: 69 files. Target was 50+, achieved.
- `SELECT TYPE`: 1 file (plus internal tests; see note on noted_issues.md).
- `FORALL`: 4 files (including 2 audit filters + 1 negative step).
- `WHERE`: 3 files.
- `ASSOCIATE`: 1 file (audit filter).
- `BLOCK` (standalone line `^\s*block$`): 2 files (one is our new reproducer).
- Total .f90 test programs: 320.
- Fixtures `tests/fixtures/`: 24 files.
- Fuzz targets: 2 (`fuzz_lexer`, `fuzz_parser`) with seed corpora.
- Fuzz smoke test: `tests/fuzz_smoke.rs` exists and runs in CI.

Notable gaps:
- No test covers a FUNCTION with an assumed-shape array argument.
- No test covers explicit-shape with dummy-arg upper bound.
- No test covers keyword-argument reordering (positional works, keyword
  reordering silently doesn't).
- No test covers nested CONTAINS host association.
- No test covers character-initializer in module/program scope.
- No test covers F2003 IMPORT statement.
- No test covers BLOCK-scope finalization.
- `SELECT TYPE` coverage remains thin; noted_issues.md already flags
  runtime-consistency volatility on the one test we have.

Tests added in this audit (all under `test_programs/audit31_brutal_*.f90`):
- `audit31_brutal_keyword_args.f90`
- `audit31_brutal_explicit_shape_bounds.f90`
- `audit31_brutal_char_init.f90`
- `audit31_brutal_nested_host.f90`
- `audit31_brutal_derived_fn_assign.f90`
- `audit31_brutal_func_assumed_shape.f90`
- `audit31_brutal_block_finalizer.f90`
- `audit31_brutal_sxtw.f90` (fails with real fortsh code; stays as the
  closest minimal we could reach)
- `audit31_brutal_crossopt_lib.f90` + `audit31_brutal_crossopt_main.f90`
  (cross-opt matrix harness — currently blocked by #2)

Recommended new tests (NOT written yet, to keep scope bounded):
- typed-allocate on derived-type component
- allocate on allocatable component of array-element base
- procedure pointer from dummy arg
- pointer-component `=>` with character substring RHS
- IMPLICIT in BLOCK
- `import ::` in interface body

## Closing recommendation

Sprint 32 should open by addressing, in this order:
1. **Finding 2** (explicit-shape bounds with dummy-arg upper) — the
   single most blocking issue. Unblocks any routine using the
   canonical `(arr, n)` BLAS signature.
2. **Finding 7** (IMPORT parse) — unblocks 6-8 fortsh files mechanically.
3. **Finding 3** (character initializer lowering) — unblocks silently-
   wrong output in every module that defines a `character, parameter ::`.
4. **Finding 1** (keyword argument reordering) — silent wrong-value is
   the worst class of compiler bug. High priority despite small LOC.
5. **Findings 8-11** (pointer-from-dummy, pointer-component-substring,
   typed-allocate, alloc-on-component) as a cluster — they all look
   like a single rework of the ALLOCATE / pointer-assignment target
   resolver.
6. **Findings 12-14** (codegen sxtw, logical-op coerce, mixed-kind
   bitwise) — each is localized and gives large .amod-cascade wins.
7. Findings 4, 5, 6 (nested host, derived-type-fn return, assumed-shape
   size-in-fn) are independent but each requires deeper lowering rework.
8. Finding 15 (BLOCK-scope finalizer) is lowest priority but needed
   before any resource-management code is considered production-ready.

Do NOT attempt "fortsh compiles" until at least items 1-6 land. Today's
8/55 → projected 35/55 with items 1-6 → realistically 50/55 with all of
the above.
