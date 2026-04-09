# ARMFORTAS Optimization Pipeline — Transfer Report

**Date**: 2026-04-09  
**Branch**: trunk @ `7a9c3c2`  
**Test status**: 879 unit tests pass, 117 end-to-end programs O0==O2, official harness green

---

## 1. Current State

The compiler has a 21-pass O2 optimization pipeline:

```
CallResolve → Mem2Reg → ConstFold → SROA → Mem2Reg(2nd) → Inline →
SimplifyCfg → DeadFuncElim → StrengthReduce → LocalLsf → LocalCse →
PreheaderInsert → LoopPeel → LoopUnswitch → Licm → ConstProp → Dse →
LoopInterchange → LoopFission → LoopFusion → LoopUnroll → GVN → Dce
```

All passes are active and verified. Two Sprint 29.8 deliverables remain incomplete:

| Pass | File | Status |
|------|------|--------|
| **GlobalLsf** | `src/opt/global_lsf.rs` | Code exists, DISABLED — path-sensitivity bug |
| **BCE** | `src/opt/bce.rs` | Elimination framework exists — needs insertion infrastructure |
| **Alias analysis** | `src/opt/alias.rs` | WORKING — used by GlobalLsf, ready for other consumers |

---

## 2. GlobalLsf: What's Wrong and How to Fix It

### The Bug

Cross-block load-store forwarding (`global_lsf.rs`) incorrectly forwards stored values across function calls and other memory-clobbering operations. Symptom: fibonacci at O2 produces extra iterations (loop bound forwarded past a recursive call that modifies the bound variable).

### Root Cause

`find_dominating_store()` walks the dominator tree from the load's block upward, looking for a store to the same address. When it finds one in a dominating block, it forwards the value. The problem: **it only checks the immediate dominator path, not all execution paths from the store to the load**.

Example:
```
Block A (dominator): store %val → %ptr
Block B (on path A→C): call foo()  ← clobbers %ptr!
Block C (load block): %r = load %ptr  ← GlobalLsf forwards %val (WRONG)
```

Block A dominates C, and A has a store to `%ptr`. But block B (which is on the path from A to C but is NOT the immediate dominator) has a call that clobbers `%ptr`. GlobalLsf doesn't check B.

### Attempted Fix (Insufficient)

Added a check for clobbers in the load_block itself before the load (lines 98-128 of `global_lsf.rs`). This catches the case where a call is in the SAME block before the load, but misses calls on INTERMEDIATE blocks between the dominator and the load.

### What LLVM Does: MemorySSA

LLVM's solution (studied from `.refs/llvm/llvm/include/llvm/Analysis/MemorySSA.h` and `.refs/llvm/llvm/lib/Analysis/MemorySSA.cpp`):

1. **Every memory operation gets a MemoryAccess node**: stores and calls are `MemoryDef`, loads are `MemoryUse`
2. **MemoryDefs form a linked list per block**: each def points to the previous def that reaches it
3. **MemoryPhi nodes at join points**: like SSA phi nodes but for memory state
4. **ClobberWalker**: to check if a store reaches a load safely, walk backward through the MemoryDef chain. For each MemoryDef encountered, ask AliasAnalysis: "does this def modify the memory location I care about?" If yes → clobbered, stop. If no → continue.

### Recommended Implementation

For our compiler, a lightweight version:

1. **Pre-pass**: Build a per-block list of memory-clobbering instructions (stores + calls)
2. **Query**: For each load, walk ALL blocks on any path from the candidate store to the load (not just the dominator chain). If ANY block on ANY path has a clobber, reject the forwarding.
3. **Conservative shortcut**: If the store's block is the immediate dominator of the load's block AND no intermediate block has a clobber, forward. Otherwise, reject.

The "walk all paths" check can use a BFS/DFS from the store's block to the load's block, checking each visited block for clobbers. This is more expensive than dominator-chain walking but correct.

### Key Files
- `src/opt/global_lsf.rs` — the pass (disabled in pipeline.rs)
- `src/opt/alias.rs` — alias oracle (working, used by GlobalLsf)
- `.refs/llvm/llvm/include/llvm/Analysis/MemorySSA.h` — LLVM's solution
- `.refs/llvm/llvm/lib/Analysis/MemorySSA.cpp` — ClobberWalker (lines 491-850)

---

## 3. BCE: What Exists and What's Needed

### What Exists

- `RuntimeFunc::CheckBounds` variant added to `src/ir/inst.rs`
- `src/opt/bce.rs` — elimination pass that removes `RuntimeCall(CheckBounds, [index, lower, upper])` when provably safe:
  - Constant index within constant bounds → eliminated
  - Loop IV within loop bounds → eliminated (basic pattern)
- Codegen support: `afs_check_bounds` symbol mapped in `isel.rs` and `printer.rs`

### What's Missing

**Bounds check insertion**: No code currently emits `RuntimeCall(CheckBounds, ...)` at array access sites. This requires:

1. **Lowerer change** (`src/ir/lower.rs`): Before each `GetElementPtr` for an array access, emit `RuntimeCall(CheckBounds, [index, lower_bound, upper_bound])` when bounds are available.
2. **Runtime function** (`src/runtime/`): Implement `afs_check_bounds(index: i64, lower: i64, upper: i64)` that aborts with a diagnostic if index is out of range.
3. **Gating**: Only emit at O0/O1. At O2+, BCE removes provably-safe checks; remaining checks stay as safety nets.
4. **Static-shape arrays**: Bounds are compile-time constants from declarations.
5. **Allocatable arrays**: Bounds must be loaded from the runtime descriptor (offsets 24/32 for dim 0 lower/upper).

### Recommended Approach

1. Start with static-shape arrays only (bounds are constants, easiest to implement and eliminate)
2. Add a `-fcheck=bounds` flag to control insertion (default on at O0, off at O2+)
3. Implement the runtime abort function with source location reporting
4. BCE then eliminates checks inside counted loops where the IV range is provably within bounds

---

## 4. Testing Methodology

### Official Test Harness

The authoritative test runner is `tests/run_programs.rs`:
```bash
cargo test -p armfortas --test run_programs
```
It supports CHECK (stdout matching), IR_CHECK/IR_NOT (IR shape), ASM_CHECK/ASM_NOT (assembly), ERROR_EXPECTED (diagnostic), XFAIL (known bugs), and EXIT_CODE annotations. **Always use this for definitive pass/fail.**

### Ad-Hoc O0 vs O2 Comparison

For quick regression checking across all programs:

```bash
# CORRECT version (handles compile failures properly):
for f in test_programs/*.f90; do
  bn=$(basename $f)
  o0_ok=true; o2_ok=true
  ./target/debug/armfortas "$f" -o /tmp/v_o0 -O0 2>/dev/null || o0_ok=false
  if $o0_ok; then timeout 5 /tmp/v_o0 > /tmp/v_o0.out 2>&1; fi
  ./target/debug/armfortas "$f" -o /tmp/v_o2 -O2 2>/dev/null || o2_ok=false
  if $o2_ok; then timeout 5 /tmp/v_o2 > /tmp/v_o2.out 2>&1; fi
  if $o0_ok && $o2_ok; then
    if diff -q /tmp/v_o0.out /tmp/v_o2.out > /dev/null 2>&1; then
      echo "PASS: $bn"
    else
      echo "MISMATCH: $bn"
    fi
  fi
done
```

**CRITICAL**: The script above uses:
- **Separate temp files per program** (`/tmp/v_o0.out`, `/tmp/v_o2.out` — fresh per iteration)
- **Proper compile-failure handling** (`|| o0_ok=false` prevents stale output)
- **Timeout** (`timeout 5` prevents infinite-output programs from filling disk)

### WRONG version (caused false "all mismatches" scare):

```bash
# DO NOT USE — stale temp files from compile failures poison subsequent diffs:
for f in test_programs/*.f90; do
  ./target/debug/armfortas "$f" -o /tmp/at_o0 -O0 2>/dev/null && /tmp/at_o0 > /tmp/at_o0.out 2>&1
  ./target/debug/armfortas "$f" -o /tmp/at_o2 -O2 2>/dev/null && /tmp/at_o2 > /tmp/at_o2.out 2>&1
  # BUG: if compilation fails, && short-circuits and /tmp/at_o2.out keeps PREVIOUS program's output
  if ! diff -q /tmp/at_o0.out /tmp/at_o2.out > /dev/null 2>&1; then
    echo "MISMATCH: $(basename $f)"  # false positive from stale file!
  fi
done
```

The `&&` between compile and run means a compile failure (from intentional ERROR_EXPECTED tests) skips the redirect, leaving the previous program's output in the file. Every subsequent diff then shows "mismatch" because it's comparing program N's O0 output against program N-1's O2 output.

### Bisecting Pass Bugs

When a mismatch is found, bisect which pass causes it:

1. **Disable suspect pass** in `src/opt/pipeline.rs` (comment out `pm.add(...)`)
2. Rebuild: `cargo build -p armfortas`
3. Test the failing program: `./target/debug/armfortas PROGRAM -o /tmp/test -O2 && /tmp/test`
4. If it passes → that pass is the bug. If still fails → try another.

For pipeline interaction bugs:
- Enable passes **one at a time** from a known-good baseline
- Run full 117-program regression after EACH addition
- The IR verifier (`verify_after_each: true` in PassManager) catches SSA violations immediately

### Assembly Determinism Check

```bash
./target/debug/armfortas FILE -S -O2 -o /tmp/det1.s
./target/debug/armfortas FILE -S -O2 -o /tmp/det2.s
diff /tmp/det1.s /tmp/det2.s
# Must produce NO output (identical files)
```

---

## 5. Reference Implementations

GCC and LLVM source are cloned in `.refs/`:
- `.refs/gcc/gcc/` — GCC compiler source
- `.refs/llvm/llvm/` — LLVM compiler source

### Key files consulted for Sprint 29.8:

| Topic | GCC File | LLVM File |
|-------|----------|-----------|
| SRA/SROA ordering | `gcc/passes.def` (lines 89, 252) | — |
| SRA implementation | `gcc/tree-sra.cc` | `llvm/lib/Transforms/Scalar/SROA.cpp` |
| Memory clobber tracking | — | `llvm/include/llvm/Analysis/MemorySSA.h` |
| MemorySSA walker | — | `llvm/lib/Analysis/MemorySSA.cpp` (lines 491-850) |
| GVN load forwarding | `gcc/tree-ssa-pre.cc` | `llvm/lib/Transforms/Scalar/GVN.cpp` (line 2161) |
| Loop fusion | `gcc/gimple-loop-jam.cc` | `llvm/lib/Transforms/Scalar/LoopFuse.cpp` |
| Loop fission | `gcc/tree-loop-distribution.cc` | `llvm/lib/Transforms/Scalar/LoopDistribute.cpp` |

### Key findings from .refs:

1. **GCC runs SRA twice**: early (after SSA, before inlining) and late (after inlining, before loops). Both times AFTER SSA construction. Our SROA now follows this pattern.
2. **LLVM MemorySSA**: every call is a MemoryDef. Before forwarding a store past a call, must ask "does this call modify my memory location?" Our GlobalLsf doesn't do this properly.
3. **GCC triggers alias rebuild after SRA**: a dummy pass executes `TODO_rebuild_alias`. We rely on the passmanager's `rebuild_type_cache()` which serves a similar purpose.

---

## 6. Sprint Roadmap

| Sprint | Status | What remains |
|--------|--------|-------------|
| 29.8 | 3/5 active | GlobalLsf (MemorySSA rework), BCE (insertion infrastructure) |
| 29.9 | Planned | NEON/SIMD vectorization (needs new IR types, ~20 machine opcodes) |
| 30 | Planned | Multi-file compilation, cross-module inlining |
| 31-35 | Planned | Integration testing, fortsh compilation milestone |

The compiler is in a stable, working state with a powerful 21-pass O2 pipeline. The remaining 29.8 work (GlobalLsf + BCE) is well-understood with clear paths forward informed by GCC/LLVM reference implementations.
