# Noted Issues

Deferred items categorized during Sprint 21.5 cleanup. Items marked **[FIXED]** were resolved in Sprint 21.5. Remaining items are categorized as:
- **(B)** Naturally resolved by a later sprint
- **(C)** Deferred to integration testing (Sprints 33-35) — will surface when compiling real codebases
- **(D)** Scheduled for .5 cleanup sprints (28.5, 29.5, or 31.5) — not skipped, just sequenced after prerequisites

---

## Lexer (Sprint 5-6)

- **[FIXED]** ~~Blank lines consumed by continuation scanning~~ → Sprint 21.5
- **(D)** Hollerith with embedded single quotes: `5HIT'SA` produces malformed string.
- **(D)** `!` inline comments in fixed-form body: accepted but non-standard F77.
- **(D)** Column offsets after whitespace stripping: span columns inaccurate in fixed-form.
- **(D)** UTF-8 multi-byte characters: both lexers process bytes, non-ASCII garbled.

## Parser (Sprint 7-8)

- **(C)** `(/ /)` array constructor: expressions inside can't contain `*` or `/`. Use `[...]` instead.
- **(D)** `AcValue::ImpliedDo` is 288 bytes. Could be boxed.
- **(D)** AssumedRank `(..)` cannot lex. F2018 assumed-rank extremely rare.
- **(C)** EnumDef has AST but no parser. Fortran enums rare.
- **(D)** Codimension attribute absent. Coarray Fortran exotic.
- **(C)** Standalone AttributeStmt (`allocatable :: x`): AST exists, no parser.

## Parser — Control Flow (Sprint 9)

- **(D)** SELECT TYPE: not implemented. fortsh has zero usage.
- **(D)** CRITICAL construct: coarray feature, not implemented.
- **(D)** Assigned GOTO: deleted from standard in F95.
- **(D)** LocalitySpec for DO CONCURRENT: F2018, rarely used.
- **(D)** Labeled DO (F77 style): parser has no label-on-DO awareness.
- **(C)** Construct name validation: names at `end do outer` not validated against opening.
- **(D)** `quiet` field on STOP/ERROR STOP: always false.

## Parser — Subprograms & Modules (Sprint 10)

- **(D)** ENTRY statement: F77 legacy, extremely rare.
- **(D)** SeparateModuleProcedure: F2008, fortsh doesn't use.
- **(D)** Statement functions: F77, ambiguous with array assignment.
- **(D)** Submodule discards imports/implicit.

## Parser — Advanced Statements (Sprint 11)

- **(B)** FORMAT mini-language → Sprint 25 (Advanced I/O).
- **(D)** Coarray statements: zero usage in fortsh.
- **(C)** I/O keyword case preservation: downstream must use eq_ignore_ascii_case.
- **[FIXED]** ~~consume_end eats trailing identifier~~ → Sprint 21.5
- **(C)** DATA statement `/` delimiter conflict: same as `(/ /)` limitation.

## Preprocessor (Sprint 4)

- **(D)** Dual codepaths for macro expansion: condition vs body expander.
- **(D)** `is_emitting()` iterates full condition stack per line.

## Semantic Analysis — Symbol Tables (Sprint 12)

- **[FIXED]** ~~Module scope re-entry mutates parent pointer~~ → Sprint 21.5 (`enter_scope`)
- **[FIXED]** ~~type_spec_to_info discards kind selectors~~ → Sprint 21.5
- **[FIXED]** ~~Function return_type propagation missing~~ → Sprint 21.5
- **(C)** USE-without-ONLY renames create duplicate associations.
- **(D)** Submodule resolution missing. fortsh has zero submodules.
- **(B)** Named interface registration → Sprint 28 (Derived Types & OOP).
- **[FIXED]** ~~Standalone PARAMETER/COMMON processing missing~~ → Sprint 29 (COMMON blocks, EQUIVALENCE). COMMON: one global per block variable; EQUIVALENCE: shared backing store with GEP aliasing. AttributeStmt still missing.
- **(C)** Ambiguous USE detection missing. `bencch` Sprint 7 graph suites now show `use collision_left_values, only: payload` together with `use collision_right_values, only: payload` compiling successfully in the same program instead of being rejected as ambiguous.
- **(B)** Module default/private access is not enforced in authored `USE` graphs → Sprint 30 (Module System). `bencch` Sprint 7 graph suites now show `use visible_values, only: hidden` compiling successfully even when the module declares `private` default access and only marks `shown` as public.
- **(B)** Renamed re-export graphs still leak the original name at import sites → Sprint 30 (Module System). `bencch` Sprint 7 graph suites now show `use bridge_aliases, only: payload` compiling successfully even though the intermediate module only imports the upstream entity as `lifted`, and the same leak appears in mixed `ONLY` graphs where a renamed import (`kept`) is still reachable later as `alpha`.
- **(C)** Default access not applied to CONTAINS subprograms.

## Semantic Analysis — Type System (Sprint 13)

- **[FIXED]** ~~Convert node insertion deferred~~ → Sprint 21.5 (implicit type conversions in lowering)
- **(B)** Component access type resolution → Sprint 28 (Derived Types & OOP).
- **(B)** Typed array constructor → Sprint 22 (Memory Management).
- **(C)** ComplexLiteral kind detection: always returns complex(4).
- **(D)** BOZ literal type context: always returns integer(4).
- **(B)** real/dble intrinsic ignores KIND argument → Sprint 26 (Intrinsics).
- Generic resolution uses exact type matching: **correct per F2018 standard** (not a bug).

## Semantic Analysis — Validation (Sprint 14)

- **(B)** Interface blocks in module specification section → Sprint 28.
- **(C)** Statement label attachment: parser doesn't attach labels to statements.
- **(C)** Pure procedure call checking: stub, only catches I/O/STOP/SAVE.
- **(C)** Defined operator intent(in) checking.
- **(C)** BLOCK construct internal declarations.
- **(B)** Elemental return type → Sprint 26 (Intrinsics).
- **(C)** Contiguous array checking.
- **(D)** SELECT TYPE validation: parser doesn't implement SELECT TYPE.

## IR — Basic Construction (Sprint 15)

- **(D)** No I128 for integer(16). Exotic.
- **(D)** `value_type()` is O(n) linear scan. Performance only.

## IR — Parameter Constants

- **[FIXED]** ~~`parameter` (named constant) declarations were lowered
  as `alloca + load` with no intervening store~~. Resolved by the
  `init_decls` pass in `src/ir/lower.rs`: after `alloc_decls` runs, a
  second pass walks declarations and emits `store(lower(init), addr)`
  for any scalar entity with `init`, plus standalone `parameter (...)`
  statement pairs. Type coercion (int↔float, width changes) happens at
  the store site. Verified at -O0/-O1/-O2/-O3 against
  `test_programs/const_prop.f90`.

## IR — Complex Lowering (Sprint 16)

- **[FIXED]** ~~ALLOCATE ignores shape arguments~~ → Sprint 21.5
- **[FIXED]** ~~Params hardcoded to Ptr(I32)~~ → Sprint 21.5 (typed from declarations)
- **[FIXED]** ~~Function call return types hardcoded to i32~~ → Sprint 21.5 (resolved from symbol table)
- **[FIXED]** ~~Runtime-variable negative DO step~~ → Sprint 21.5 (runtime sign check)
- **[FIXED]** ~~ASSOCIATE leaks bindings into outer scope~~ → Sprint 21.5
- **[FIXED]** ~~Integer literal truncation~~ → Sprint 21.5 (kind-aware emission)
- **(B)** Module globals never referenced at use-site → Sprint 30 (Module System). `bencch` Sprint 7 multi-file graph suites now show this directly on executable module/use cases too: imported module values in authored graphs like `module_chain`, `rename_only`, `visibility`, `fanin`, `reexport_chain`, `diamond_merge`, `mixed_only`, `collision_shadow`, and `layered_leaves` lower to zero-initialized globals in IR and print `0` at runtime instead of the expected imported values, including multi-hop, shared-dependency, mixed-import, and larger layered multi-leaf graphs.
- **(B)** Module procedures in authored graphs are called but not lowered/emitted → Sprint 30 (Module System). `bencch` Sprint 7 graph suites now show IR calls like `call @add_one(...)` without a matching `func @add_one`, backend artifacts that branch to `_add_one` without ever emitting it, and final link failures from the missing symbol.
- **(B)** No derived type lowering → Sprint 28 (Derived Types & OOP).
- **(B)** DoConcurrent silently dropped → fortsh doesn't use (D). PointerAssignment → Sprint 28. Read/I/O → Sprint 24-25.

## Codegen (Sprints 17-21)

- **(B)** Stack-passed arguments (>8 args): silently dropped. Rare for Fortran scalars.
- **(B)** Register hint population: infrastructure present, not wired to isel.
- **(C)** Callee-saved frame growth ordering undocumented (works by late-binding sentinel).
- **(C)** Non-deterministic backend/codegen output across identical builds: repeated `-S`, repeated `-c`, repeated `armfortas::testing` `asm` capture, and repeated `armfortas::testing` `obj` capture on the same source and opt level can all emit different register choices and text bytes. `bencch` consistency suites also show repeated capture-vs-CLI mismatches for both `asm` and `obj`; on the current backend fixtures the capture path often stays `0/3` aligned with repeated CLI rebuilds across the sampled matrix. `-S | as` vs `-c` mismatches remain text-only, and relocations/load commands/symbols stay stable while text changes. Runtime consistency coverage in `bencch` is green on bench-owned backend fixtures and across an expanding real-program corpus covering numerics, arrays/control flow, derived types, fixed and deferred strings, repeated string reassignment, file I/O, `rewind`, flush-heavy I/O, and `bind(c)` interop, so the observed nondeterminism is currently below the sampled runtime-behavior surface rather than showing up as output or exit-code drift there. Blocks reproducible builds and “binary is exactly what we expect” confidence.
- **(C)** `SELECT TYPE` runtime consistency is currently volatile even when observable output is supposed to be simple and deterministic. In `bencch`, repeated full CLI builds, captured `run` behavior, and capture-vs-CLI comparisons for `test_programs/select_type.f90` can disagree across `O0` through `Ofast`, with observed variants including missing `CLASS IS` output, alternate first lines, empty output, and occasional nonzero CLI exits during repeat-based sampling. The current standalone test source is also rejected by `gfortran` because the selector is not polymorphic, so this is armfortas-local consistency coverage rather than a clean differential case today. Blocks high-confidence runtime-level assertions for this source until SELECT TYPE lowering/codegen/runtime interaction is stabilized.
- **(C)** Register width mismatch in array index GEP: `mul x24, w23, x26` mixes 32-bit and 64-bit registers. The GEP index needs width promotion before multiplication. Surfaces when passing arrays to subroutines with assumed-shape/assumed-rank args.
- **(C)** Array intrinsic return type width: SIZE/SUM return i64 but assigning to integer(4) variable stores 8 bytes into 4-byte slot. Needs truncation at assignment site when intrinsic result is wider than target. Direct printing via PRINT works correctly.

## IR Lowering — Sprint 29 Audit Findings

- **[FIXED]** ~~ComplexLiteral expressions not lowered~~ → `Expr::ComplexLiteral` arm added to `lower_expr_full`. Complex stored as `[f32/f64 x 2]` on stack; assignment uses memcpy; arithmetic (add/sub/mul) correct; `afs_write_complex_f32/f64` runtime functions added for list-directed I/O.
- **[FIXED]** ~~Stack frames larger than ~32KB produce broken prologue/epilogue~~ → `emit.rs` StpPre/LdpPost arms now use x9 scratch register to synthesize the address (`add x9, sp, #N; str x29/x30`) when `stp_offset > 32760`.

## Optimizer — Sprint 29 Deferred (pipeline.rs)

- **(D)** `-O3`/`-Ofast` run identical passes to `-O2`. Accepted at the flag level; correctness is preserved but no extra optimization. Deferred: NEON vectorization, aggressive inlining, IPO, devirtualization, fast-math reassociation.
- **[FIXED]** ~~Function inlining: not implemented~~ → Sprint 29.7 delivered call resolution, call graph, cost model, inline pass, SimplifyCFG, and dead function elimination. Inlining is one-at-a-time per pass manager iteration (safe). Multi-site-per-invocation was attempted but caused SSA breakage when two calls in the same block were split; reverted.
- **(D)** Inline performance: one call site per pass manager iteration means O(iterations) for functions with many call sites. With `max_iterations=32`, functions with >~10 inlinable sites may not fully inline. The correct optimization is re-scanning the modified function within a single invocation (not batching all sites upfront, which breaks SSA). Deferred to a future sprint.
- **(D)** GVN (global value numbering), SROA (scalar replacement of aggregates) — deferred.
- **[FIXED]** ~~DSE, loop unrolling, FMADD/FMSUB fusion~~ → Sprint 29 delivered all three. DSE is intra-block only; loop unroll handles trip count ≤ 8 with single IV; FMADD/FMSUB peephole fuses single-use fmul into fma.
- **(D)** LDP/STP pair merging beyond prologue/epilogue: current pairing is callee-save-only in `linearscan.rs`. General body-level load/store pair merging not implemented.
- **(D)** Tail call optimization: implementation correct (`tailcall.rs`), but the pattern (Bl immediately before callee-restore cluster + LdpPost + Ret in same block) rarely fires because Fortran tail calls are typically inside conditionals, placing the Bl and epilogue in separate blocks. Not a bug — inert until cross-block TCO or block merging is added.

## Derived Types & OOP (Sprint 28)

- **[FIXED]** Component access read/write including chained (x%a%b) and by-ref params.
- **[FIXED]** Structure constructors with zero-init and arg count warnings.
- **[FIXED]** Type extension (EXTENDS) with inherited field access.
- **[FIXED]** Type-bound procedures with PASS/NOPASS semantics.
- **[FIXED]** FINAL procedures at scope exit (the gfortran ARM64 crash).
- **[FIXED]** SELECT TYPE with TYPE IS and CLASS IS guards.
- **[FIXED]** Allocatable component layout (384-byte descriptor slots).
- **(D)** Vtable-based virtual dispatch — requires CLASS variable descriptor infrastructure (polymorphic ALLOCATE, indirect calls through vtable function pointers). Foundation in place: type tags, parent tracking. Needed for true polymorphic dispatch.
- **(D)** Allocatable component ALLOCATE/DEALLOCATE — layout correct but no codegen to route ALLOCATE to embedded descriptor.
- **(D)** Type mismatch validation in component assignment — needs sema validation pass.

---

## Summary

| Category | Count | Description |
|----------|-------|-------------|
| **[FIXED]** | 41 | Sprint 21.5 (14), Sprint 25/25.5 (11), Sprint 26 (8), Sprint 27 (5), Sprint 29 (3) |
| **(B)** Naturally resolved | 13 | Covered by Sprints 26.5, 28.7, and remaining plan |
| **(C)** fortsh-blocking | 14 | Defer to integration (Sprints 33-35) |
| **(D)** Exotic/rare | 19 | Keep noted, no planned work |
| **(D)** Optimizer deferred | 5 | O3/Ofast parity, inlining, GVN/SROA, body-level LDP/STP, cross-block TCO |
