# x09 IR pass audit — hidden target assumptions

Sprint x09, deliverable 1. One row per pass registered in
`build_pipeline` (`src/opt/pipeline.rs:120-287`), pipeline order, union
over levels. Four questions per pass:

- **cost** — reasons about instruction cost or code size?
- **align/vec** — assumes an alignment or vector width?
- **sym** — names symbols in a platform-prefixed form?
- **libm** — assumes libm identities?

Verdicts: `neutral` (no target coupling), `tuned-arm` (correct
everywhere, thresholds tuned on ARM output), `arm-only` (must not run
for x86).

| pass | cost | align/vec | sym | libm | verdict |
|---|---|---|---|---|---|
| CallResolve | no | no | no — matches external call names against same-module IR function names; both bare on all targets (`call_resolve.rs:23-41`) | no | neutral |
| Mem2Reg | no | no | no | no | neutral |
| ConstFold | no | no | no | folds `FSqrt`/`FPow` IR ops via Rust f64 builtins (`const_fold.rs:193`), not named libm calls; IEEE-identical on both targets | neutral |
| Sroa | `SROA_MAX_FIELDS = 8` (`sroa.rs:21`) — structural cap, not a cost model | no | no | no | neutral |
| Inline | thresholds 20/100/200 per level (`inline.rs:26-39`), IR-instruction counts; tuned while benchmarking ARM64 output | no | no | no | tuned-arm (retune deferred, see below) |
| ConstArgSpecialize | no | no | no | no | neutral |
| DeadArgElim | no | no | no | no | neutral |
| ReturnPropagate | no | no | no | no | neutral |
| SimplifyCfg | no | no | no | no | neutral |
| DeadFuncElim | no | no | `__prog_*` prefix check (`dead_func.rs:34` et al.) — internal lowering convention, identical on both targets | no | neutral |
| Bce | no | no | no | no | neutral |
| StrengthReduce | no | no | no | no | neutral |
| LocalLsf | no | layout via `module.layout` param, delegated to alias oracle — no constants | no | no | neutral |
| GlobalLsf | no | same as LocalLsf | no | no | neutral |
| LocalCse | no | no (the `128` at `cse.rs:109` is an i128 sign-extension width, not a lane width) | comment mentions ADRP+ADD (`cse.rs:54`) — commentary only, no name matching | treats FSqrt/FPow as generic pure IR ops | neutral |
| PreheaderInsert | no | no | no | no | neutral |
| LoopPeel | no | no | no | no | neutral |
| LoopUnswitch | `UNSWITCH_MAX_BODY = 50` (`unswitch.rs:38-39`) — bloat cap on IR count | no | no | no | tuned-arm (cap is IR-level; keep shared) |
| Licm | no | no | no | treats Call/RuntimeCall as side-effecting, no name matching | neutral |
| Sccp_ | no | no | no | no | neutral |
| JumpThread | no | no | no | no | neutral |
| ConstProp | no | no | no | no | neutral |
| Dse | no | layout param for alias queries only | no | no | neutral |
| LoopInterchange | legality-gated (GEP/dependence), no cost model | no | no | no | neutral |
| LoopFission | `FISSION_MIN_BODY = 4` (`fission.rs:20`) — structural floor | no | no | no | neutral |
| LoopFusion | structural thresholds only | no | no | no | neutral |
| NeonVectorize | shape-matching, no cost model | **yes — NEON 128-bit lanes hardcoded**: I32/F32→4, I64/F64→2 (`neon_vectorize.rs:1001-1008`); emits V-ops only arm64 isel selects | no | recognizes FSqrt/FAbs as IR ops, not names | **arm-only** (gate off x86; x10 owns the SSE story) |
| Vectorize | kernel availability per element type (I32/F32/F64 only) | offloads lanes to runtime kernels | calls `afs_fill_*`/`afs_array_*_*` by bare name (`vectorize.rs:560-606`); prefixing is emitter-only on both backends | no | **arm-only this sprint** — kernels are NEON-backed in the runtime; gate off x86 until x10 validates them |
| LoopUnroll | `FULL_UNROLL_MAX = 8`, `DO_CONCURRENT_FULL_UNROLL_MAX = 16`, `BODY_SIZE_MAX = 30` (`unroll.rs:83-92`), partial budget 60 / factor 4 (`unroll.rs:1094-1096`) — IR counts, ARM-tuned | no | no | no | tuned-arm (retune deferred, see below) |
| FastMathReassoc | no | no | no | no — pure IR reassociation | neutral (registers at Ofast only, both targets) |
| Gvn | fixpoint cap 8 iterations, not a cost model | no — vector ops opt out of GVN (`gvn.rs:595-616`) | whitelist names are bare | **yes — `PURE_EXTERNAL_INTRINSICS` whitelist of bare libm names** (`gvn.rs:120-176`): sinf…copysign. Verified: IR call names are unprefixed on both targets; the Mach-O underscore is applied only in `arm64/emit.rs` (`:837-846`, `:855-859`), x86 `symbol()` is identity. Safe. | neutral |
| Dce | no | treats VStore as side-effecting (correct on any target) | no | no | neutral |

## Findings

1. **Only the two vectorizers are target-coupled.** `NeonVectorize`
   emits 128-bit V-ops sized to NEON lanes; `Vectorize` calls runtime
   kernels that are NEON-backed today. Both register only at O3/Ofast.
   Action (deliverable 2): gate both off non-arm64 in `build_pipeline`.
   Loudly: the x86 O3/Ofast pipeline records the omission in its
   pass-name golden test, not silently.
2. **No symbol-prefix leaks.** Every name an opt pass sees is bare IR;
   prefixing happens only in the emitters. The gvn whitelist and the
   vectorize kernel names are correct on both targets as-is.
3. **No alignment assumptions outside the vectorizers.** Layout
   reasoning is delegated to `module.layout`/alias oracle everywhere.

## Cost-model retune deferrals (recorded, not acted on)

Per the sprint pitfall list, IR-level thresholds stay shared and
untouched this sprint; revisit with x10 benchmark data:

- `inline.rs:26-39` thresholds (20/100/200) — x86 two-address form
  inflates post-regalloc instruction count; the IR-count proxy skews
  differently per target.
- `unroll.rs:83-92,1094-1096` caps — same proxy concern, plus -Os size
  measurements shift on x86.
- `unswitch.rs:38` body cap — low risk, same rationale.

## FMA contraction policy

ARM64 contracts `fmul+fadd → fmadd` in the backend peephole at O2+
(`src/codegen/arm64/peephole.rs`), changing last-ulp results (single
rounding). x86_64 baseline is SSE2: no FMA instruction exists, so no
contraction — and none may be added until a capability-gated FMA3 path
lands (x10 or later). Policy:

- FP contraction is a **backend** decision, never an IR pass. IR-level
  FastMathReassoc remains Ofast-only on both targets.
- Cross-target output equality for FP programs is therefore only
  guaranteed where contraction doesn't bite. New fixtures that compare
  accumulated FP output must print at a stable precision; existing
  macOS assertions are never weakened (sprint rule).
- The x86 peephole (deliverable 4) must not synthesize x87 or FMA;
  the x87 grep gate in CI enforces the former permanently.

## Allocator decision (intake item, recorded)

The x07-era assumption was that -O1+ on x86 had to wait for a real
register allocator. The x09 sweeps falsified that: the naive
allocator is correct at every level (533/2/0 across the corpus,
-O0..-Ofast, FreeBSD + Linux), so the per-level gates opened without
allocator work. The x86 linear scan — and with it the MirView
shared-core question from x05 — is deferred to x10, where the
benchmark gate provides the measurements that performance work
should answer to. Recorded in x10's intake.

## Backend pass dispatch (deliverable 3 fact-check)

The sprint doc's driver line references are stale: the x03 backend
split already moved the post-isel passes into
`arm64::emit_module` (`src/codegen/arm64/mod.rs:31-71` — peephole at
O2+, tailcall at O1+ inside the linear-scan path, relax_branches
unconditional). The x86 path in `codegen::emit_module`
(`src/codegen/mod.rs:36-84`) shares none of it, and the two backends
have distinct MIR types, so running an ARM pass on x86 MIR is already
a compile error. Deliverable 3 is satisfied by construction; what
remains is adding the x86 counterpart passes (peephole — deliverable
4) and a real x86 register allocator (the matrix work), since the x86
path currently runs `regalloc_naive` at every level.
