# Sprint 29.6: Loop Optimizations

## Prerequisites
Sprint 29 (Optimization Passes — LICM, const prop, DSE must be in place)

## Goals
Implement loop-level transformations that exploit Fortran's regular loop structure and
contiguous array semantics. Fortran gives us aliasing guarantees C lacks — use them.

## Deliverables

### 1. Loop Fusion
Merge adjacent loops over the same range into one:
```fortran
do i = 1, n
    a(i) = b(i) + 1.0
end do
do i = 1, n
    c(i) = a(i) * 2.0
end do
! →
do i = 1, n
    a(i) = b(i) + 1.0
    c(i) = a(i) * 2.0
end do
```
Requires: identical bounds, no loop-carried dependence from second loop reading before
first loop writes. Improves cache behavior and opens vectorization opportunities.

### 2. Loop Fission
Split loops with independent bodies that have poor cache behavior:
```fortran
do i = 1, n
    a(i) = b(i) + c(i)
    x(i) = y(i) * z(i)
end do
! → split into two loops if a/b/c and x/y/z are unrelated
```
Trade-off: more loops, but each accesses fewer distinct arrays. Gate on array count
threshold (split if > 3 distinct arrays, or when dependence analysis shows no sharing).

### 3. Loop Interchange
Swap nested loop order for better memory access patterns. Critical for Fortran because
arrays are column-major:
```fortran
! Before: inner loop strides over columns (cache-hostile)
do j = 1, m
    do i = 1, n
        a(i, j) = b(i, j) + 1.0
    end do
end do
! After: inner loop strides over rows (cache-friendly for column-major)
do i = 1, n
    do j = 1, m
        a(i, j) = b(i, j) + 1.0
    end do
end do
```
Requires: no loop-carried dependences between i and j iterations (standard array
assignments without cross-iteration reads are safe). This is the single highest-impact
loop transform for Fortran.

### 4. Loop Peeling
Peel first/last iterations to remove boundary checks or special cases:
```fortran
! If first iteration has a special case (e.g., a(1) = 0), peel it out
! so the loop body is uniform and vectorizable
```
Also enables prefetch insertion at the peeled iterations.

### 5. Loop Unswitching
Hoist loop-invariant conditionals out of loops:
```fortran
do i = 1, n
    if (flag) then
        a(i) = b(i)
    else
        a(i) = c(i)
    end if
end do
! →
if (flag) then
    do i = 1, n; a(i) = b(i); end do
else
    do i = 1, n; a(i) = c(i); end do
end if
```
Condition: `flag` must be provably loop-invariant (not written in the loop body,
not loaded from an aliased address).

### 6. NEON/SIMD Vectorization
Vectorize inner loops over contiguous arrays using ARM64 NEON:
- `real(4)` arrays: 4-wide FADD/FMUL/FSUB/FDIV on v0.4s
- `real(8)` arrays: 2-wide on v0.2d
- `integer(4)` arrays: 4-wide ADD/MUL on v0.4s
- Reduction: use FADDP/ADDV for final horizontal reduce

Requirements:
- Loop body contains only elementwise operations (no cross-element reads)
- Array accesses are unit-stride (inner dimension)
- Trip count ≥ vector width (peel remainder for non-multiple counts)
- No aliasing between input and output arrays (Fortran INTENT gives us this)
- Gate at O2+; enable `-ftree-vectorize`-equivalent flag

Supported patterns:
```fortran
do i = 1, n
    c(i) = a(i) + b(i)     ! FADD v0.4s, v1.4s, v2.4s
    d(i) = a(i) * b(i)     ! FMUL
    e(i) = a(i) * b(i) + c(i)  ! FMLA (fused)
end do
```

Whole-array assignment (`a = b + c`) lowers to a vectorized loop. This pairs with the
array expression lowering from sprint 28.7.

## Algorithm Notes

### Dependence Analysis
All loop transforms require proving no loop-carried dependences. For Fortran array
accesses `a(f(i))` vs `a(g(i))`:
- If f and g are affine (linear in i), use GCD test (a(3i+1) and a(3i+2) never alias)
- If f == g (same subscript), there's a RAW/WAW dependence — block fusion/interchange
- Conservative: treat any pointer-derived access as dependent

### Alias Analysis Fortran Bonus
Fortran dummy arguments with INTENT(IN) cannot alias INTENT(OUT) arguments.
Fortran prohibits pointer aliasing between local arrays (no `equivalence` across
different arrays in modern Fortran). These rules let us vectorize more aggressively
than C compilers can.

### Vectorization Decision Heuristic
1. Count loop body operations
2. If ≥ 4 elementwise FP ops on same arrays: vectorize
3. Check alignment (AArch64 doesn't require aligned loads but 16-byte aligned is faster)
4. Emit scalar remainder loop for `n mod width` tail

## Testing Strategy
- Correctness: every loop test program produces identical results at O0 vs O2
- Interchange: matrix multiply before/after must produce same values
- Vectorization: verify NEON instructions appear in `-S` output; benchmark shows speedup
- Fusion: two-loop sum equals one-loop sum
- Fission: split loop produces same outputs
- Regression: existing test suite passes at all opt levels

## Definition of Done
- Loop fusion fires for same-range adjacent loops with no dependences
- Loop interchange fires for column-major inner loops in O2+ mode
- Loop fission fires for threshold-crossing bodies with no sharing
- Loop peeling fires for boundary special-cases
- Loop unswitching fires for invariant conditionals
- NEON vectorization fires for unit-stride elementwise loops on real arrays
- All transforms gated behind appropriate opt level flags
- Correctness invariant: all existing tests pass at O0/O1/O2/O3
