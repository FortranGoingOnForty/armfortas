# Sprint 29.9: Fortran-Specific & Interprocedural Optimizations

## Prerequisites
Sprint 29.7 (inlining — required for IPO), Sprint 29.8 (alias analysis — required for
Fortran-specific opts), Sprint 30 (multi-file — required for whole-program analysis)

## Goals
Exploit Fortran's unique language properties to achieve optimization quality that C
compilers structurally cannot match. Then implement interprocedural analysis that
crosses function and module boundaries for whole-program optimization.

## Deliverables

### 1. No-Alias Exploitation
Fortran's scoping and INTENT rules mean most variables don't alias. Codify this in a
Fortran-aware alias analysis layer on top of sprint 29.8's base AA:

- **Dummy argument non-aliasing**: the Fortran standard prohibits aliasing between
  actual arguments unless the program is non-conforming. Mark each pair of dummy
  arguments as `NoAlias` by default (unless they're known POINTER or TARGET).
- **Local variable non-aliasing**: stack-allocated locals cannot alias each other or
  any dummy argument (unless passed to a subprogram by reference — track this).
- **Module variable non-aliasing**: module variables accessed by USE don't alias local
  variables unless an association is established via POINTER assignment.

This means: in a DO loop over `a` and `b`, we can vectorize without alias checks even
when the function prototype looks like C would require `restrict`. Use this to fire
vectorization (sprint 29.6) more aggressively than gfortran's conservative C-AA.

### 2. PURE/ELEMENTAL Exploitation
Functions declared PURE have no side effects (no global modification, no I/O, no
observable state change). Exploit this:

- **CSE across PURE calls**: two calls to the same PURE function with the same arguments
  have the same result. Replace the second with the first's result.
  ```fortran
  x = sin(theta)    ! computed
  y = sin(theta)    ! → y = x (if theta unchanged)
  ```
- **Reordering**: PURE function calls can be reordered with each other and with
  non-aliasing loads/stores. Enable better scheduling.
- **Speculative evaluation**: a PURE call result can be precomputed before the branch
  that decides whether it's needed (if the cost is low).

ELEMENTAL functions additionally guarantee element-independence — combine with
vectorization to auto-vectorize ELEMENTAL calls over array arguments.

### 3. Whole-Array Operation Vectorization
When the IR sees an array assignment lowered from sprint 28.7's whole-array ops:
```fortran
a = b + c    ! lowered to: do i = 1, size(a); a(i) = b(i) + c(i); end do
```
Recognize this pattern and emit a single vectorized loop (using NEON from sprint 29.6)
rather than a scalar counted loop. The loop is always unit-stride and the arrays are
non-aliasing (Fortran guarantees this for whole-array ops).

### 4. DO CONCURRENT Parallelization
`DO CONCURRENT` guarantees no loop-carried dependences. Exploit this:
- **Vectorization**: always safe — no inter-iteration deps by spec
- **Prefetching**: emit software prefetch hints for the next cache line
- **Reordering**: iterations can execute in any order — schedule for pipeline efficiency
- **Future**: annotate for OpenMP/SIMD when targeting multi-core (deferred to sprint 35+)

At minimum: ensure the compiler exploits DO CONCURRENT's vectorization license at O2+.

### 5. Interprocedural Optimizations (IPO)

#### 5a. Dead Argument Elimination
If a function argument is never read by the callee (after inlining and DCE), remove it
from the function signature and all call sites. Common after aggressive inlining exposes
dead branches.

#### 5b. Constant Argument Propagation
When all call sites pass the same value for an argument, specialize:
```fortran
call compute(a, 1)    ! all callers pass n=1
call compute(b, 1)
! → specialize compute_n1 with n=1 propagated → const fold downstream
```

#### 5c. Return Value Propagation
If a function always returns the same constant (detectable post-inlining + const prop),
replace all call results with that constant.

#### 5d. Devirtualization
For PROCEDURE arguments and type-bound procedures where the static type is known,
replace the indirect call with a direct call. Enables further inlining.

#### 5e. Whole-Program Analysis
When all source files are given at once (single-invocation compilation):
1. Build a module-spanning call graph
2. Apply IPO passes across the entire graph
3. Detect dead public procedures (referenced by no call site) and elide them
4. Detect module variables never written by any procedure — treat as constant

### 6. Optimization Interaction
These passes have a natural ordering that maximizes their combined impact:
1. Whole-program call graph construction
2. Devirtualization (turns indirect calls into direct calls)
3. Cross-module inlining (sprint 29.7)
4. Constant argument propagation + return value propagation
5. Dead argument elimination
6. Per-function passes (const prop, DCE, GVN, DSE) on the specialized callees
7. Loop vectorization with Fortran AA (no-alias + DO CONCURRENT exploitation)
8. PURE/ELEMENTAL exploitation in the vectorized loops

## Testing Strategy
- No-alias: compile a loop over two INTENT(IN) arrays; verify vectorization fires (would
  require `restrict` hint in C to achieve the same)
- PURE CSE: compile a program calling `sin(x)` twice with same arg; verify one call in output
- DO CONCURRENT: verify vectorized NEON instructions appear for a DO CONCURRENT loop
- IPO dead arg: compile with unused argument; verify signature shrinks post-optimization
- IPO const arg: verify specialization fires when all callers pass the same value
- Devirtualization: verify type-bound procedure call becomes `bl my_func` not `blr`

## Definition of Done
- Fortran dummy-argument non-aliasing enables more vectorization than conservative AA
- PURE function CSE eliminates duplicate calls with identical arguments
- DO CONCURRENT loops vectorize without alias bailout
- IPO: dead arguments eliminated from all-call-site-dead signatures
- IPO: constant argument propagation fires for homogeneous call sites
- IPO: devirtualization fires for statically-known type-bound calls
- Whole-program analysis runs when all sources given in single invocation
- All passes gated at appropriate -O level (O2 for Fortran AA + PURE; O3 for IPO)
- All existing tests pass at O0 through O3
