# Sprint 29.8: Advanced IR Optimizations

## Prerequisites
Sprint 29 (basic opt passes: const prop, DCE, DSE, CSE), Sprint 29.6 (loop opts useful
but not required)

## Goals
Implement the remaining IR-level optimization passes that require deeper analysis:
GVN (which subsumes and extends CSE), SROA (aggregate decomposition), alias analysis,
load-store forwarding, and array bounds check elimination. These passes together
constitute the bulk of what separates an "optimizing" compiler from one that merely
does peephole work.

## Deliverables

### 1. Global Value Numbering (GVN)
GVN detects equivalent computations across different control flow paths, beyond what
local CSE can see:
```fortran
if (cond) then
    x = a + b      ! VN: add(a,b) = 42
else
    y = a + b      ! same VN — reuse value from if-branch
end if
z = a + b          ! post-join: GVN knows a+b = 42 on both paths
```

Algorithm (RPO-based with hash-consing):
1. Process blocks in reverse post-order (dominator order)
2. Assign value numbers to each instruction result based on opcode + operand VNs
3. If two instructions have the same VN, replace the second with a reference to the
   first (or its dominating definition via a lookup table)
4. Handle phi/block-parameter equivalences: if both predecessors feed the same value
   into a block param, the param is equivalent to that value

GVN subsumes local CSE — once GVN is in place, the local CSE pass can be retired or
kept as a fast pre-pass.

### 2. Scalar Replacement of Aggregates (SROA)
Decompose small structs and arrays that are only accessed by component into individual
scalar variables:
```fortran
real :: p(2)    ! alloca [2 x float]
p(1) = 3.0
p(2) = 4.0
r = p(1) + p(2)
! → after SROA:
! p_0 = 3.0; p_1 = 4.0; r = p_0 + p_1
! (no alloca, no loads/stores — mem2reg can finish the job)
```

Criteria for SROA eligibility:
- All accesses to the alloca are GEP with constant indices (statically knowable component)
- No address escape (the alloca address is never passed to a function or stored)
- No whole-aggregate copy (`memcpy` of the alloca counts as escape)

For complex numbers (stored as `[2 x float]` allocas), SROA will naturally decompose
them into two scalar floats — enabling the register allocator to keep re/im in fp regs.

### 3. Alias Analysis
A dedicated alias analysis pass to inform GVN, load-store forwarding, and vectorization.
Fortran gives us strong aliasing guarantees:

- **No pointer arithmetic**: Fortran pointers must point to whole objects. No
  `ptr + offset` like C.
- **INTENT(IN) vs INTENT(OUT)**: these cannot alias in a conforming program.
- **Local arrays**: stack-allocated arrays cannot alias each other unless passed to
  a subprogram that makes them alias (Fortran prohibits this).
- **Module variables**: a module variable and a dummy argument can alias only if
  explicitly associated.

Represent alias results:
```rust
enum AliasResult { MustAlias, MayAlias, NoAlias }
fn alias(a: &MemRef, b: &MemRef, aa: &AliasAnalysis) -> AliasResult
```

Use alias results to: enable more load-store forwarding, unlock loop fusion across
loads/stores that were previously blocked, and let vectorization proceed without
conservative bailout.

### 4. Load-Store Forwarding (Extended)
Extends the within-block load-store forwarding from sprint 29 to across-block forwarding
using alias analysis and dominance:

```
block A:
    store %val → %ptr        ; store VN(ptr) = VN(val)
block B (dominated by A):
    %r = load %ptr            ; no store to ptr between A and B → %r = %val
```

Requirements:
- A dominates B
- No store to an aliasing address on any path from A to B
- `%ptr` itself not modified between A and B

Cross-block forwarding pairs with GVN: the forwarded value gets a value number and
can be further propagated.

### 5. Array Bounds Check Elimination
When we can prove an array index is within bounds, elide the runtime bounds check:

```fortran
do i = lbound(a,1), ubound(a,1)
    a(i) = 0.0    ! i provably in [lbound, ubound] — no check
end do
```

Requires:
- Loop variable range analysis: track `[lo, hi]` interval for each induction variable
- Array descriptor access: `lbound` and `ubound` from the descriptor
- Prove `lo >= lbound` and `hi <= ubound` symbolically

Simple cases first:
- Loop from `1` to `n` where `a` has declared extent `n` → no check needed
- Loop from `lbound(a,1)` to `ubound(a,1)` → trivially safe

Harder cases deferred:
- Non-unit strides, multi-dimensional with section strides — conservative bail-out

Emit a compile-time diagnostic when -O0 bounds checks are enabled for safety.

## Algorithm Summary

| Pass | Input | Output | Key Invariant |
|------|-------|--------|---------------|
| GVN | IR function | Redundant insts removed | Dominance tree, hash table |
| SROA | alloca insts | Scalars replacing aggregates | No address escape |
| Alias Analysis | IR module | AliasResult for any pair | Fortran scoping rules |
| Load-Store Fwd | IR function + AA | Loads replaced by stored values | Dominance + no-alias |
| Bounds Check Elim | IR function | Removed bounds checks | Range analysis |

## Testing Strategy
- GVN: compile program with `a + b` computed multiple times under different branches;
  verify one computation after GVN
- SROA: compile program using complex number components; verify after SROA that no
  alloca remains for the complex vars (mem2reg finishes the job)
- Alias analysis: verify INTENT(IN)/INTENT(OUT) args marked NoAlias in IR
- Load-store forwarding: verify no redundant load after a store to same addr in -S output
- Bounds check elim: verify `do i = 1, n; a(i) = 0.0` emits no bounds-check call
- Correctness: full test suite passes at all opt levels

## Definition of Done
- GVN eliminates redundant computations across basic block boundaries
- SROA decomposes constant-indexed aggregates into scalars
- Alias analysis classifies Fortran dummy arg intent correctly
- Cross-block load-store forwarding uses alias analysis to avoid false forwarding
- Bounds check elimination fires for canonical counted loops
- All passes run as part of the O2 pipeline
- IR verifier passes after each new pass
- All existing tests pass at O0 through O3
