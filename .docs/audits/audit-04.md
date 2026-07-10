# Optimization Pass Audit 04

Date: 2026-07-09
Branch: `adversarial-audit-20260709`
Commit: `23857aa48f3bc0160303842488e8578acb487fb1`
Scope: optimizer correctness, pass ordering, alias assumptions, floating-point environment boundaries, deterministic output, termination, and compile-time scaling.

The tests below used the release compiler at `/tmp/armfortas-audit/target/release/armfortas`. All generated sources, executables, assembly, and IR were kept under the unique directory `/tmp/armfortas-audit04.Rkhj0e`. No implementation, test, CI, submodule, or reviewer-owned file was changed.

Common setup for the reproductions:

```sh
cd /tmp/armfortas-audit04.Rkhj0e
AFS=/tmp/armfortas-audit/target/release/armfortas
```

## Confirmed Correctness Findings

### A04-01: Zero-argument calls do not invalidate global load/store facts

- **Severity:** High
- **Confidence:** High
- **Affected levels:** `-O1`, `-O2`, `-O3`, `-Os`, and `-Ofast`

**Source evidence**

- `src/opt/lsf.rs:117-143` collects pointer arguments at a call and invalidates memory facts only when that list is nonempty.
- `src/opt/global_lsf.rs:201-221` applies the same rule to global load/store forwarding.
- `src/opt/dse.rs:76-99` also treats calls without pointer arguments as preserving pending stores.

A call can modify module, `SAVE`, or `COMMON` state without receiving a pointer argument. The legality rule therefore confuses an empty argument list with an empty memory effect.

**Exact reproduction**

`call_global.f90`:

```fortran
module state_mod
  implicit none
  integer :: x = 0
  abstract interface
    subroutine action()
    end subroutine action
  end interface
contains
  subroutine set_x()
    x = 2
  end subroutine set_x
end module state_mod

program call_global
  use state_mod
  implicit none
  procedure(action), pointer :: p
  integer :: observed

  p => set_x
  x = 1
  call p()
  observed = x
  print '(i0)', observed
end program call_global
```

```sh
for opt in O0 O1 O2 O3 Os Ofast; do
  "$AFS" "-$opt" call_global.f90 -o "call_global_$opt"
  printf '%s: ' "$opt"
  "./call_global_$opt"
  "$AFS" "-$opt" --emit-ir call_global.f90 -o "call_global_$opt.ir"
done
gfortran -O3 call_global.f90 -o call_global_gfortran
./call_global_gfortran
```

**Observed behavior**

```text
O0: 2
O1: 1
O2: 1
O3: 1
Os: 1
Ofast: 1
gfortran -O3: 2
```

The `-O0` IR stores `1`, performs the indirect call, and reloads the module variable. Optimized IR replaces the post-call load with constant `1`.

**Expected behavior**

Every level must print `2`. An unknown or indirect call must conservatively invalidate facts for global and otherwise externally reachable memory unless a sound memory-effect summary proves that it cannot modify them.

**Impact**

Ordinary calls to procedures that update module, `SAVE`, or `COMMON` variables can produce stale values or cause observable stores to be removed. Procedure pointers and separately compiled callees make the defect especially easy to expose.

### A04-02: Loop fission assumes different array bases are independent

- **Severity:** High
- **Confidence:** High
- **Affected levels:** `-O2`, `-O3`, and `-Ofast`

**Source evidence**

- `src/opt/fission.rs:68-90` selects loops with writes to distinct base values.
- `src/opt/fission.rs:92-105` skips same-base memory-reference pairs and asks dependence analysis only about references whose bases differ.
- `src/opt/dep_analysis.rs:239-243` immediately reports references with different base values as independent.

The fission gate therefore cannot detect a producer/consumer dependence from one array to another, even though separating the statements changes their schedule.

**Exact reproduction**

`fission_cross_array.f90`:

```fortran
program fission_cross_array
  implicit none
  integer :: a(5), b(5), i

  a = -99
  b = 0
  b(1) = 5
  do i = 2, 5
    if (i >= 2) then
      a(i) = b(i - 1)
      b(i) = a(i) + 1
    end if
  end do
  print '(5(i0,1x))', a
  print '(5(i0,1x))', b
end program fission_cross_array
```

```sh
for opt in O0 O1 O2 O3 Os Ofast; do
  AFS_VERIFY_AFTER_EACH=1 "$AFS" "-$opt" \
    fission_cross_array.f90 -o "fission_cross_array_$opt"
  printf '%s:\n' "$opt"
  "./fission_cross_array_$opt"
  AFS_VERIFY_AFTER_EACH=1 "$AFS" "-$opt" --emit-ir \
    fission_cross_array.f90 -o "fission_cross_array_$opt.ir"
done
gfortran -O3 fission_cross_array.f90 -o fission_cross_array_gfortran
./fission_cross_array_gfortran
```

**Observed behavior**

```text
O0/O1/Os and gfortran -O3:
-99 5 6 7 8
5 6 7 8 9

O2/O3/Ofast:
-99 5 0 0 0
5 6 1 1 1
```

The failing optimized IR contains a `fission_bridge` and a cloned loop. Per-pass verification accepts the transformed IR, so this is a semantic legality failure rather than malformed IR.

**Expected behavior**

The loop must retain the original statement order for each iteration, yielding `a = [-99,5,6,7,8]` and `b = [5,6,7,8,9]`. Fission is legal only after proving that no dependence crosses the proposed partition.

**Impact**

Loops that move data between distinct arrays can silently compute from stale values after fission.

### A04-03: Loop fusion treats distinct pointer descriptors as non-aliasing arrays

- **Severity:** Critical
- **Confidence:** High
- **Affected levels:** Observed at `-O2`; the same fusion pass is present at `-O3` and `-Ofast`

**Source evidence**

- `src/opt/dep_analysis.rs:316-339` collects references from the two loops and skips a pair when the references have different base `ValueId`s.
- `src/opt/fusion.rs:134-168` trusts that result when deciding whether to fuse and then rewrites the loop schedule.

Distinct SSA values for Fortran pointer descriptors do not imply disjoint targets. Both pointers below designate the same target.

**Exact reproduction**

`fusion_pointer_alias.f90`:

```fortran
program fusion_pointer_alias
  implicit none
  integer, target :: storage(5)
  integer, pointer :: p(:), q(:)
  integer :: i

  storage = -1
  p => storage
  q => storage
  do i = 1, 4
    if (i >= 1) p(i) = 100 + i
  end do
  do i = 1, 4
    if (i >= 1) q(i) = q(i + 1)
  end do
  print '(5(i0,1x))', storage
end program fusion_pointer_alias
```

The fusion candidate selection is also nondeterministic as described in A04-04, so repeat compilation to expose the illegal fused form:

```sh
rm -f fusion_alias_*.bin fusion_alias_*.out
for i in $(seq -w 1 50); do
  AFS_VERIFY_AFTER_EACH=1 "$AFS" -O2 fusion_pointer_alias.f90 \
    -o "fusion_alias_$i.bin"
  "./fusion_alias_$i.bin" > "fusion_alias_$i.out"
done
sort fusion_alias_*.out | uniq -c
gfortran -O3 fusion_pointer_alias.f90 -o fusion_pointer_alias_gfortran
./fusion_pointer_alias_gfortran
```

**Observed behavior**

Two armfortas results occur. The unfused form prints the correct result:

```text
102 103 104 -1 -1
```

The fused form prints:

```text
-1 -1 -1 -1 -1
```

In the recorded 20-build sample, 11 binaries produced the correct result and 9 produced the incorrect result. `gfortran -O3` produced the correct result.

**Expected behavior**

Every compilation must print `102 103 104 -1 -1`. Fusion must conservatively account for pointer association and potential overlap; different descriptor SSA values are not a no-alias proof.

**Impact**

Fusion can silently miscompile valid pointer-based loops by exposing values from a future iteration before the first loop has initialized them.

### A04-04: Fusion candidate selection makes IR, binaries, and behavior nondeterministic

- **Severity:** High
- **Confidence:** High
- **Affected levels:** Observed at `-O2`

**Source evidence**

- `src/opt/loop_tree.rs:21` stores a natural loop body in a `HashSet<BlockId>`.
- `src/opt/fusion.rs:404-415` iterates that set without sorting and returns the first block containing any integer comparison.

The test loop has both a loop-bound comparison and an inner `if` comparison. Rust's randomized `HashSet` iteration can select either one, changing whether fusion's shape checks succeed.

**Exact reproduction**

Use `fusion_pointer_alias.f90` from A04-03:

```sh
rm -f fusion_sample_*
for i in $(seq -w 1 20); do
  "$AFS" -O2 --emit-ir fusion_pointer_alias.f90 -o "fusion_sample_$i.ir"
  "$AFS" -O2 -S fusion_pointer_alias.f90 -o "fusion_sample_$i.s"
  "$AFS" -O2 fusion_pointer_alias.f90 -o "fusion_sample_$i.bin"
  "./fusion_sample_$i.bin" > "fusion_sample_$i.out"
done

sha256sum fusion_sample_*.ir  | awk '{print $1}' | sort | uniq -c
sha256sum fusion_sample_*.s   | awk '{print $1}' | sort | uniq -c
sha256sum fusion_sample_*.bin | awk '{print $1}' | sort | uniq -c
sort fusion_sample_*.out | uniq -c
```

**Observed behavior**

- The 20 emitted IR files had two distinct hashes, distributed 5 and 15.
- The 20 assembly files had two distinct hashes, distributed 5 and 15.
- The 20 executables had two distinct hashes, distributed 11 and 9.
- Runtime output was correct in 11 runs and incorrect in 9 runs.

All commands had identical source, flags, compiler binary, working directory, and serial execution environment.

**Expected behavior**

Repeated compilation with identical inputs must select the same legal transformation and emit deterministic IR and machine code. Legality must not depend on randomized container iteration.

**Impact**

The same build can alternate between correct and incorrect executables. This prevents reproducible builds and makes the underlying fusion miscompile intermittent in CI and user builds.

### A04-05: Bounds-check elimination discards narrowing integer-cast semantics

- **Severity:** High
- **Confidence:** High
- **Affected levels:** `-O2`, `-O3`, `-Os`, and `-Ofast` with `-fcheck=bounds`

**Source evidence**

- `src/opt/bce.rs:82-106` strips integer casts before deriving the index range used to remove a check.
- `src/opt/bce.rs:337-356` treats `IntExtend` and `IntTrunc` identically in `strip_int_casts`.

Extension can preserve the represented integer, but truncation can wrap it into a completely different range. Proving the pre-truncation range is in bounds does not prove the actual array index is in bounds.

**Exact reproduction**

`bce_trunc.f90`:

```fortran
program bce_trunc
  use iso_fortran_env, only: int8
  implicit none
  integer :: a(200), i

  a = 0
  do i = 129, 130
    a(int(i, int8)) = 7
  end do
  print '(a)', 'survived'
end program bce_trunc
```

```sh
set +e
for opt in O0 O1 O2 O3 Os Ofast; do
  "$AFS" "-$opt" -fcheck=bounds bce_trunc.f90 -o "bce_trunc_$opt"
  "./bce_trunc_$opt" > "bce_trunc_$opt.out" 2>&1
  status=$?
  printf '%s exit=%s output=' "$opt" "$status"
  tr '\n' ' ' < "bce_trunc_$opt.out"
  printf '\n'
  "$AFS" "-$opt" -fcheck=bounds --emit-ir bce_trunc.f90 \
    -o "bce_trunc_$opt.ir"
done
gfortran -O2 -fcheck=bounds bce_trunc.f90 -o bce_trunc_gfortran
./bce_trunc_gfortran
printf 'gfortran exit=%s\n' "$?"
set -e
```

**Observed behavior**

```text
O0/O1: exit 1, Bounds check failed: index -127 outside [1, 200]
O2/O3/Os/Ofast: exit 0, survived
gfortran -O2 -fcheck=bounds: nonzero exit, index -127 diagnosed
```

The `-O0` and `-O1` IR retain the bounds check. `-O2`, `-O3`, `-Os`, and `-Ofast` remove it.

**Expected behavior**

`int(129, int8)` is `-127`, so every checked build must diagnose an index outside `[1,200]`. BCE must either model the range after truncation or decline the proof.

**Impact**

Optimization defeats an explicitly requested runtime safety check and permits out-of-bounds memory access.

### A04-06: Constant folding ignores the active IEEE rounding mode

- **Severity:** High
- **Confidence:** High
- **Affected levels:** Strict semantics are broken at `-O1`, `-O2`, `-O3`, and `-Os`; `-Ofast` shows the same transformation but may intentionally relax FP semantics

**Source evidence**

- `src/opt/const_fold.rs:186-194` evaluates floating arithmetic with ordinary Rust host operations.
- `src/opt/const_fold.rs:556-615` scans and folds instructions without tracking floating-point environment state.

The pass is ordered early in every optimized pipeline, before later passes could preserve a dynamic rounding boundary.

**Exact reproduction**

`fpenv_constfold.f90`:

```fortran
program fpenv_constfold
  use, intrinsic :: ieee_arithmetic
  use iso_fortran_env, only: real64, int64
  implicit none
  real(real64) :: a, b, rounded_up

  a = 1.0_real64
  b = 5.5511151231257827e-17_real64
  call ieee_set_rounding_mode(ieee_up)
  rounded_up = a + b
  call ieee_set_rounding_mode(ieee_nearest)
  print '(z16.16)', transfer(rounded_up, 0_int64)
end program fpenv_constfold
```

```sh
for opt in O0 O1 O2 O3 Os Ofast; do
  "$AFS" "-$opt" fpenv_constfold.f90 -o "fpenv_constfold_$opt"
  printf '%s: ' "$opt"
  "./fpenv_constfold_$opt"
  "$AFS" "-$opt" --emit-ir fpenv_constfold.f90 \
    -o "fpenv_constfold_$opt.ir"
done
gfortran -O2 -frounding-math fpenv_constfold.f90 -o fpenv_constfold_gfortran
./fpenv_constfold_gfortran
```

**Observed behavior**

```text
O0:    3FF0000000000001
O1:    3FF0000000000000
O2:    3FF0000000000000
O3:    3FF0000000000000
Os:    3FF0000000000000
Ofast: 3FF0000000000000
gfortran -O2 -frounding-math: 3FF0000000000001
```

The `-O0` IR retains an `fadd` after the rounding-mode setter. Optimized IR replaces the expression with floating constant `1.0`.

**Expected behavior**

Under upward rounding, `1.0 + 2^-54` must produce the next representable `real64`, bit pattern `3FF0000000000001`. Strict optimization levels must not evaluate the expression using compile-time round-to-nearest.

**Impact**

Programs using `ieee_set_rounding_mode` can receive numerically wrong compile-time constants at every strict optimized level.

### A04-07: CSE/GVN recognize floating-environment barriers only by direct callee name

- **Severity:** High
- **Confidence:** High
- **Affected levels:** `-O1`, `-O2`, `-O3`, and `-Os`; also observed at `-Ofast`

**Source evidence**

- `src/opt/cse.rs:254-267` disables FP CSE only after finding a direct call whose external name is a known IEEE rounding/status helper.
- `src/opt/gvn.rs:450-491` uses the same direct-name barrier test.
- `src/opt/gvn.rs:656-665` otherwise allows equivalent FP expressions to share a value across calls.

An indirect call or an ordinary wrapper can change the floating-point environment without exposing the helper's name in the caller.

**Exact reproduction**

`fpenv_indirect.f90`:

```fortran
program fpenv_indirect
  use, intrinsic :: ieee_arithmetic
  use iso_fortran_env, only: real64, int64
  implicit none
  abstract interface
    subroutine action()
    end subroutine action
  end interface
  procedure(action), pointer :: p
  integer :: n
  real(real64) :: a, b, rounded_down, rounded_up

  n = command_argument_count()
  a = real(n + 1, real64)
  b = 5.5511151231257827e-17_real64

  p => set_down
  call p()
  rounded_down = a + b
  p => set_up
  call p()
  rounded_up = a + b
  p => set_nearest
  call p()

  print '(z16.16,1x,z16.16)', transfer(rounded_down, 0_int64), &
    transfer(rounded_up, 0_int64)
contains
  subroutine set_down()
    call ieee_set_rounding_mode(ieee_down)
  end subroutine set_down

  subroutine set_up()
    call ieee_set_rounding_mode(ieee_up)
  end subroutine set_up

  subroutine set_nearest()
    call ieee_set_rounding_mode(ieee_nearest)
  end subroutine set_nearest
end program fpenv_indirect
```

```sh
for opt in O0 O1 O2 O3 Os Ofast; do
  "$AFS" "-$opt" fpenv_indirect.f90 -o "fpenv_indirect_$opt"
  printf '%s: ' "$opt"
  "./fpenv_indirect_$opt"
  "$AFS" "-$opt" --emit-ir fpenv_indirect.f90 \
    -o "fpenv_indirect_$opt.ir"
done
```

**Observed behavior**

```text
O0:    3FF0000000000000 3FF0000000000001
O1:    3FF0000000000000 3FF0000000000000
O2:    3FF0000000000000 3FF0000000000000
O3:    3FF0000000000000 3FF0000000000000
Os:    3FF0000000000000 3FF0000000000000
Ofast: 3FF0000000000000 3FF0000000000000
```

Optimized IR contains one `fadd` whose result is reused for both outputs. The direct IEEE helper calls are inside wrappers; the caller contains only indirect calls.

**Expected behavior**

The first addition must be rounded down to `3FF0000000000000`; the second must be rounded up to `3FF0000000000001`. An unknown call is an FP-environment barrier unless interprocedural effects prove otherwise.

**Impact**

Strict optimized code can reuse an FP result across arbitrary calls that change rounding or status, producing stale numeric values even when the expression itself is not constant.

## Confirmed Performance and Termination Findings

### A04-08: One-site-at-a-time inlining drives superlinear compilation and silently stops after 32 sites

- **Severity:** Medium
- **Confidence:** High

**Source evidence**

- `src/opt/inline.rs:134-138` deliberately inlines only the first eligible call site found in one pass invocation.
- `src/opt/pass.rs:35-45` sets a hard global pipeline limit of 32 iterations.
- `src/opt/pass.rs:79-97` reruns the entire optimization pipeline while any pass reports a change.
- `src/driver/mod.rs:1686` does not inspect the returned `PassRunResult`, so reaching the cap is silent.

**Exact reproduction**

```sh
gen_inline() {
  n=$1
  out="inline_${n}.f90"
  {
    printf '%s\n' \
      'module inline_mod' \
      'contains' \
      '  integer function bump(x)' \
      '    integer, intent(in) :: x' \
      '    bump = x * 3 + 1' \
      '  end function bump' \
      'end module inline_mod' \
      'program inline_scaling' \
      '  use inline_mod' \
      '  implicit none' \
      '  integer :: seed, total' \
      '  seed = command_argument_count()' \
      '  total = 0'
    for k in $(seq 1 "$n"); do
      printf '  total = total + bump(seed + %d)\n' "$k"
    done
    printf '%s\n' \
      "  print '(i0)', total" \
      'end program inline_scaling'
  } > "$out"
}

for n in 4 8 16 32 64 128 256 512; do
  gen_inline "$n"
  for rep in 1 2 3; do
    start=$(date +%s%N)
    "$AFS" -O2 --emit-ir "inline_${n}.f90" -o "inline_${n}_${rep}.ir"
    end=$(date +%s%N)
    printf '%s %s %s\n' "$n" "$rep" "$(((end-start)/1000000))"
  done
done

for n in 32 64 128 256 512; do
  printf 'N=%s remaining bump calls=' "$n"
  rg -c 'call.*bump' "inline_${n}_1.ir" || printf '0\n'
done
```

**Observed behavior**

Serial `-O2 --emit-ir` wall times in milliseconds:

| Calls | Recorded range | Calls remaining in IR |
|---:|---:|---:|
| 4 | 2.6-2.7 | 0 |
| 8 | about 3.7 | 0 |
| 16 | about 7.5, one 12.4 outlier | 0 |
| 32 | 22-29 | 0 |
| 64 | 52-55 | 32 |
| 128 | 159-169 | 96 |
| 256 | 582-596 | 224 |
| 512 | 2502-2534 | 480 |

Exactly 32 sites are inlined once the cap is reached. Doubling this isolated case from 256 to 512 calls increased compile time by approximately 4.25 times.

**Expected behavior**

Inlining should process eligible sites with a worklist or equivalent bounded local algorithm instead of rerunning every pass for one site. If a safety cap prevents convergence, the compiler should not silently present the result as a completed fixpoint.

**Impact**

Large call-heavy procedures experience superlinear compile time and receive optimization results that depend on source call-site order. This is independent evidence from an isolated serial microbenchmark; the transient failures in the concurrent integration run are not treated as evidence for this finding.

### A04-09: Dead-store elimination performs quadratic pairwise alias checks for independent stores

- **Severity:** Medium
- **Confidence:** High

**Source evidence**

- `src/opt/dse.rs:42-67` keeps prior stores in a `Vec` and compares each new store against every pending store. Independent constant-offset stores remain pending, producing a triangular number of alias queries.

**Exact reproduction**

```sh
gen_dse() {
  n=$1
  out="dse_${n}.f90"
  {
    printf '%s\n' \
      'program dse_scaling' \
      '  implicit none'
    printf '  integer :: a(%d)\n' "$n"
    for k in $(seq 1 "$n"); do
      printf '  a(%d) = %d\n' "$k" "$k"
    done
    printf '%s\n' \
      "  print '(i0)', a($n)" \
      'end program dse_scaling'
  } > "$out"
}

for n in 200 400 800 1600 3200; do
  gen_dse "$n"
  for opt in O1 O2; do
    for rep in 1 2 3; do
      start=$(date +%s%N)
      "$AFS" "-$opt" --emit-ir "dse_${n}.f90" -o "dse_${n}_${opt}_${rep}.ir"
      end=$(date +%s%N)
      printf '%s %s %s %s\n' "$opt" "$n" "$rep" \
        "$(((end-start)/1000000))"
    done
  done
done
```

**Observed behavior**

Serial wall-time ranges in milliseconds:

| Stores | `-O1` | `-O2` |
|---:|---:|---:|
| 200 | 10-13 | about 11 |
| 400 | 21-28 | about 29 |
| 800 | about 50 | about 81 |
| 1600 | 114-116 | 260-297 |
| 3200 | 282-288 | 917-973 |

At `-O2`, doubling from 1600 to 3200 stores costs approximately 3.5 times as much. The source structure and pending-store loop account for the quadratic optimizer component; parsing and lowering contribute additional approximately linear work.

**Expected behavior**

Independent fixed-offset stores should be indexed or partitioned so the pass does not compare every store with every preceding store.

**Impact**

Large generated procedures and explicit initialization tables can spend disproportionate compile time in DSE at optimization levels that include the pass.

## Coverage Gaps

- Runtime validation was performed on the available host target only. AArch64-generated code was not executed, so target-specific backend behavior remains outside this audit.
- The audit used focused differential cases and emitted-IR comparisons, not exhaustive randomized differential testing across the full Fortran language.
- Alias probes covered module state, procedure pointers, Fortran data pointers, and distinct-array loop dependences. Coarrays, polymorphic descriptors, cross-translation-unit summaries, and all `COMMON`/`EQUIVALENCE` combinations were not exhaustively exercised.
- Analysis-cache invalidation was inspected around the global fixpoint driver and stress-tested indirectly with `AFS_VERIFY_AFTER_EACH=1`, but no standalone invalidation defect was confirmed. There is no pass-by-pass semantic oracle for stale analyses.
- Floating-point tests covered dynamic rounding through direct, wrapped, and indirect calls. Exception-flag observability, signaling NaNs, contraction, and target-specific excess precision were not exhaustively classified.
- `-Ofast` may intentionally relax floating-point semantics. A04-06 and A04-07 are reported as strict-mode defects based on their independent reproduction at `-O1`, `-O2`, `-O3`, and `-Os`.
- The one-off concurrent integration failures in `compile_scaling_defop` and `compile_scaling_inline` are excluded as contention. A04-08 and A04-09 rely only on separately repeated serial microbenchmarks with scaling tied to specific source loops.

## Finding Summary

| ID | Severity | Area | Confirmed result |
|---|---|---|---|
| A04-01 | High | LSF/global LSF/DSE alias effects | Zero-argument call miscompiles module-state reload |
| A04-02 | High | Loop fission dependence | Cross-array producer/consumer loop computes stale values |
| A04-03 | Critical | Loop fusion alias legality | Aliased pointer loops can fuse into wrong code |
| A04-04 | High | Determinism/fusion selection | Identical builds produce two IR forms and two runtime results |
| A04-05 | High | Bounds-check elimination | Narrowing cast causes a required bounds check to disappear |
| A04-06 | High | Constant folding/FP environment | Dynamic upward rounding is replaced by round-to-nearest constant |
| A04-07 | High | CSE/GVN/FP environment | FP expression is reused across indirect rounding-mode changes |
| A04-08 | Medium | Inlining/fixpoint termination | Superlinear compile time and silent 32-site truncation |
| A04-09 | Medium | Dead-store elimination | Pairwise pending-store scan shows quadratic scaling |
