# Sprint 29.7: Function Inlining

## Prerequisites
Sprint 29 (IR + opt pipeline), Sprint 30 (multi-file compilation — needed for cross-module inlining)

## Goals
Implement function inlining at multiple thresholds, from small PURE functions at O1
through cross-module aggressive inlining at O3. Inlining is the key enabler for
downstream optimizations: once a call is inlined, const prop, DSE, and loop opts
can fire across the former call boundary.

## Deliverables

### 1. Basic Inlining (O1+)
Inline small functions below a size threshold:
- Threshold: ≤ 20 IR instructions at O1, ≤ 50 at O2
- Criteria:
  - Not recursive (no self-calls or cycles in the call graph)
  - PURE or ELEMENTAL — always safe to inline (no side effects)
  - Single call site — always worth inlining (eliminates the call entirely)
- Algorithm: copy callee IR into caller at the call site; substitute arguments for
  parameters; redirect return value to the call result vreg

```fortran
pure function square(x) result(y)
    real, intent(in) :: x
    real :: y
    y = x * x
end function square

! a = square(b) → after inlining:
! tmp = b * b; a = tmp
```

### 2. Inlining Cost Model
Measure inlinability by IR instruction count (call-independent heuristic):
- Count: each `Inst` except `Alloca`, `Ret`, `Br` counts as 1
- Memory ops (`Load`, `Store`) count as 1
- `RuntimeCall` counts as 10 (expensive, discourages inlining around I/O)
- Function calls within the callee count as 5 (indirect cost)

Inline if: `cost ≤ threshold × call_frequency_weight`

At O3, frequency weight boosts threshold for hot loops (calls inside a loop body × 5).

### 3. Threshold Inlining (O2+)
Same algorithm as basic, but higher thresholds:
- ≤ 100 IR instructions at O2
- ≤ 200 IR instructions at O3
- Apply to: any non-recursive function, regardless of purity
- Caller penalty: inlining a 100-instruction callee 10 times = 1000 extra instructions —
  track total code growth and cap at 3× original caller size

### 4. Aggressive Inlining (-O3)
Push beyond threshold for hot-path functions:
- Inline into loop bodies even if callee exceeds threshold
- Speculative inlining: inline if profiling data (or heuristic — loop-enclosed call site)
  suggests the call is hot
- Inline recursive functions up to a fixed unrolling depth (depth ≤ 3)
- After inlining, re-run const prop + DCE to clean up: inlining often exposes constant
  arguments that unlock folding

### 5. Cross-Module Inlining (O3 + requires Sprint 30)
When all source files are given in one `afs` invocation:
- Read callee IR from the compiled `.amod` file (add IR section to the .amod format)
- Apply same inlining decision as within-module
- Cross-module candidates: small utility modules, math helpers, type constructors
- PURE/ELEMENTAL functions from USE'd modules are prime candidates

### 6. Inlining and the Call Graph
Build a call graph before inlining:
```rust
struct CallGraph {
    nodes: HashMap<FunctionId, CallNode>,
}
struct CallNode {
    callees: Vec<FunctionId>,
    callers: Vec<FunctionId>,
    ir_cost: usize,
    is_recursive: bool,
}
```
Process in reverse post-order (callees before callers) so inlined callee IR has already
had its callees inlined before we encounter the caller. This enables transitive inlining
without multiple passes.

### 7. Inline Substitution Algorithm
1. Clone callee IR (deep copy, fresh ValueIds)
2. Map callee parameter ValueIds to caller argument ValueIds (or fresh copies for
   by-value args)
3. Map callee alloca slots to new slots in caller frame
4. Replace `Ret` instruction with assignment to result slot + jump to post-call block
5. Insert cloned insts at the call site
6. Delete the `Call` instruction

For by-reference arguments (the normal Fortran case): pass the caller's alloca address
directly. No copy needed. If the callee has INTENT(IN), the no-modify guarantee makes
this safe even at the IR level.

## Testing Strategy
- Unit test: inline a 3-instruction PURE function, verify expanded IR
- Integration: compile program using small pure functions at O1; verify `-S` shows no `bl`
  to those functions
- Cross-module: compile two files in one invocation; verify callee inlined at O3
- No-inline guard: recursive functions must not be inlined
- Code growth cap: verify inlining stops when total growth threshold is hit
- Correctness: all test programs produce same output at O0 and O3

## Definition of Done
- PURE functions ≤ 20 insts inlined at O1
- All non-recursive functions ≤ 50 insts inlined at O2
- Hot-path functions inlined aggressively at O3
- Call graph built before inlining; traversal is correct
- Recursive functions never inlined
- Cross-module inlining works at O3 with multi-file input
- Code growth capped at 3× caller size
- All existing tests pass at all opt levels
