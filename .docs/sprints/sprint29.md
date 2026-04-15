# Sprint 29: Optimization Passes

## Prerequisites
Sprint 21 (register allocation), Sprint 16 (IR complete)

## Goals
Implement optimization passes that transform our IR to produce code on par with GCC -O3. We're building a compiler, not a toy — if gfortran can do it, so can we. Every GCC optimization that's applicable to Fortran is in scope, including aggressive and experimental passes. Bespoke means best-in-class.

## Deliverables

### 1. Constant Folding
Evaluate constant expressions at compile time:
```
%x = iadd const(3), const(4)   →   %x = const(7)
%y = fmul const(2.0), const(3.14)  →   %y = const(6.28)
%z = icmp.eq const(5), const(5)    →   %z = const(true)
```

Also fold through conversions:
```
%x = int_to_float const_int(42)    →   %x = const_float(42.0)
```

### 2. Constant Propagation
Replace uses of variables known to be constant:
```
%x = const(42)
%y = iadd %x, %z    →    %y = iadd const(42), %z
```

Then constant folding can kick in if `%z` is also constant.

### 3. Dead Code Elimination (DCE)
Remove instructions whose results are never used:
```
%x = iadd %a, %b       ; no uses of %x → remove
%y = call @side_effect  ; has side effects → keep
```

Also remove unreachable blocks (blocks with no predecessors except the entry block).

### 4. Common Subexpression Elimination (CSE)
```
%a = iadd %x, %y
%b = iadd %x, %y       →    %b = %a (reuse %a)
```

Within a basic block (local CSE) and across dominating blocks (global CSE).

### 5. Loop-Invariant Code Motion (LICM)
Move computations that don't change across loop iterations to before the loop:
```
; Before:
loop:
    %n = load @global_n        ; n doesn't change in loop
    %x = iadd %i, %n
    ...

; After:
    %n = load @global_n        ; hoisted before loop
loop:
    %x = iadd %i, %n
    ...
```

Requires alias analysis to prove that loads are loop-invariant.

### 6. Strength Reduction
Replace expensive operations with cheaper ones:
- Multiply by power of 2 → shift left
- Divide by power of 2 → shift right (unsigned) or arithmetic shift (signed positive)
- Multiply by constant → shift-and-add sequence
- `i * 2 + i` → `i * 3` → shift + add

### 7. Basic Inlining
Inline small functions (below a threshold, e.g., < 20 IR instructions):
```fortran
pure function square(x) result(y)
    real, intent(in) :: x
    real :: y
    y = x * x
end function
```

Inlining this avoids function call overhead. Criteria:
- Function is small
- Called frequently (or called once in a hot loop)
- No recursion
- PURE functions are safe to inline

### 8. Array Bounds Check Elimination
When we can prove an index is in bounds, skip the bounds check:
```fortran
do i = 1, n
    a(i) = 0.0    ! i is always in [1, n] = bounds of a → no check needed
end do
```

This requires:
- Array bounds known at compile time or tracked symbolically
- Loop variable range analysis
- Simple cases: loop from lbound to ubound of the same array

### 9. Tail Call Optimization
Convert tail calls to jumps:
```fortran
recursive function walk(node) result(val)
    if (.not. associated(node%next)) then
        val = node%data
    else
        val = walk(node%next)     ! tail call → jump
    end if
end function
```

ARM64: replace `bl + ldp + ret` with just `b` (reuse current frame).

### 10. Aggressive Inlining (-O3)
Go beyond small-function inlining:
- Inline functions up to ~200 IR instructions
- Inline into hot loops even if the function is large
- Inline across module boundaries (using .amod info)
- Speculative inlining for polymorphic calls when the type is statically known
- Full recursive inlining for small recursive functions (unroll recursion)

### 11. Loop Optimizations (-O2/-O3)
- **Loop unrolling**: Unroll small loops (trip count known, body small) by factor of 2/4/8
- **Loop fusion**: Merge adjacent loops over the same range
- **Loop fission**: Split loops with independent bodies for better cache behavior
- **Loop interchange**: Swap nested loop order for better memory access patterns (column-major!)
- **Loop vectorization**: Use ARM64 NEON/SIMD instructions for data-parallel loops (FADD v0.2d, etc.)
- **Loop peeling**: Peel first/last iterations to simplify loop body
- **Loop unswitching**: Hoist conditionals out of loops when invariant

### 12. Interprocedural Optimizations (-O3)
- **Whole-program analysis**: When all sources given at once, analyze across modules
- **Devirtualization**: Resolve polymorphic calls to direct calls when type is known
- **Dead argument elimination**: Remove arguments that are never used by any caller
- **Constant argument propagation**: If all callers pass the same value, specialize
- **Return value propagation**: If return value is always the same, replace with constant

### 13. Memory Optimizations
- **Scalar replacement of aggregates (SROA)**: Decompose small structs/arrays into individual variables
- **Global value numbering (GVN)**: Subsumes CSE with more power (detects equivalent computations through different paths)
- **Dead store elimination**: Remove stores that are overwritten before being read
- **Load-store forwarding**: Replace a load with the value that was just stored to the same address
- **Alias analysis**: Fortran's lack of pointer aliasing (INTENT, no pointer arithmetic) gives us stronger alias information than C — exploit this

### 14. ARM64-Specific Optimizations
- **NEON vectorization**: Use 128-bit SIMD for array operations (2x f64, 4x f32, 4x i32)
- **Conditional select**: Use CSEL/CSINC instead of branch for simple conditionals
- **Fused multiply-add**: FMADD/FMSUB for `a*b + c` patterns (1 cycle instead of 2)
- **Address mode exploitation**: ARM64 has rich addressing modes (pre/post-index, register offset with shift) — use them
- **Load/store pair**: Merge adjacent loads/stores into LDP/STP
- **Branch prediction hints**: Use instruction scheduling to help the branch predictor

### 15. Fortran-Specific Optimizations
- **Array contiguity exploitation**: Fortran guarantees arrays are contiguous (unless section) — use memcpy for array assignment, vectorize confidently
- **No-alias guarantees**: Fortran's scoping rules mean most variables don't alias — much stronger optimization than C
- **PURE/ELEMENTAL exploitation**: Pure functions can be CSE'd, reordered, and parallelized freely
- **DO CONCURRENT parallelization**: Mark DO CONCURRENT loops for SIMD or multi-threaded execution
- **Whole-array operations**: `a = b + c` where a, b, c are arrays → single vectorized loop

### 16. Optimization Levels
```
-O0    No optimization (default during development)
-O1    Constant folding, DCE, basic CSE, copy propagation
-O2    All of -O1 + LICM, inlining (small), strength reduction, bounds check elim,
       GVN, SROA, dead store elim, loop unrolling (small), FMA fusion
-O3    All of -O2 + aggressive inlining, loop vectorization (NEON), loop interchange,
       loop fusion/fission, interprocedural optimization, devirtualization,
       whole-program analysis, speculative optimizations
-Os    Like -O2 but prefer size (no unrolling, less inlining)
-Ofast -O3 + fast-math (reassociate, no NaN/Inf checks, reciprocal approximations)
```

### 17. Pass Manager
```rust
struct PassManager {
    passes: Vec<Box<dyn Pass>>,
}

trait Pass {
    fn name(&self) -> &str;
    fn run(&self, module: &mut IrModule) -> bool;  // returns true if IR changed
}

impl PassManager {
    fn run(&self, module: &mut IrModule) {
        let mut changed = true;
        while changed {
            changed = false;
            for pass in &self.passes {
                changed |= pass.run(module);
                verify_ir(module);  // verify after every pass
            }
        }
    }
}
```

Run verification after every pass — if an optimization introduces a bug, we catch it immediately.

## Testing Strategy

### Correctness First
**Every program that compiles correctly at -O0 must produce identical results at -O1, -O2, and -O3.** This is the #1 invariant. Test with the full existing test suite at each optimization level.

### Optimization Verification
For each pass, verify that it actually fires:
- Compile with pass enabled, dump IR before and after
- Verify the transformation occurred
- Verify output is still correct

### Compile-Time Performance
Measure compilation time with and without optimizations. Optimizations should not make compilation unreasonably slow.

### Runtime Performance
Benchmark programs (matrix multiply, sorting, numerical integration, BLAS-like kernels) at -O0 through -O3:
- -O1 should be noticeably faster than -O0
- -O2 should be faster than -O1
- -O3 should match or beat `gfortran -O3` on ARM64

This IS a gate. Compare against gfortran -O3 output on the same benchmarks. Target: within 10% of gfortran -O3 performance, with the ambition to exceed it by exploiting Fortran's aliasing guarantees more aggressively than GCC does.

### Regression Tests
Any bug found in optimized code → write a test case that reproduces it, add to the regression suite, fix the optimization.

## Definition of Done
- Constant folding, propagation, GVN, and SROA work
- Dead code elimination and dead store elimination work
- Common subexpression elimination works (local and global)
- Loop optimizations: LICM, unrolling, interchange, fusion, vectorization (NEON)
- Strength reduction for power-of-2 multiply/divide and constant multiplies
- Aggressive inlining (small functions at -O1, large functions at -O3, cross-module at -O3)
- Array bounds check elimination for provably-safe cases
- Tail call optimization for self-recursive functions
- ARM64-specific: FMADD fusion, CSEL lowering, LDP/STP merging, NEON vectorization
- Fortran-specific: no-alias exploitation, PURE/ELEMENTAL reordering, whole-array vectorization
- Interprocedural: devirtualization, dead argument elimination, constant argument propagation
- -O0, -O1, -O2, -O3, -Os, -Ofast levels all work
- All existing tests pass at ALL optimization levels (correctness invariant)
- -O3 benchmarks within 10% of gfortran -O3 (performance gate)
- IR verifier runs after every pass
- `cargo test` optimization tests pass
