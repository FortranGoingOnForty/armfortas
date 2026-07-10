# ARM64 code-generation audit

Reviewed implementation commit `23857aa48f3bc0160303842488e8578acb487fb1` on an x86_64 Linux host. The review covered Apple ARM64 argument/result classes, instruction selection, NZCV lifetime, liveness and linear scan, frame layout, tail calls, vector selection, peepholes, branch relaxation, assembly emission, and the generated-assembly-to-Mach-O object path.

No ARM64 executable was run. Reproduction therefore uses emitted assembly, the compiler's optimized IR, Clang's Apple-target ABI output, LLVM assembly validation, and the in-tree assembler's Mach-O output. The focused command `cargo test -p armfortas --lib codegen::arm64::` passed all 125 selected tests. No full workspace suite was run.

## Confirmed discrepancies

### 1. Post-call i128 result loads are mistaken for restores and moved before the call

- **Severity:** High
- **Source:** `src/codegen/arm64/tailcall.rs:64-83` skips every `LdpOffset`, `LdrImm`, or `LdrFpImm` between a `Bl` and the epilogue without proving it is a callee-save restore. `src/codegen/arm64/isel.rs:2372-2382` legitimately emits an `LdpOffset` after a call to place an i128 function result in `x0:x1`.
- **Reproducer:**

  ```sh
  target/debug/afs -ffree-form --target arm64-macos -O1 -S \
    -o /dev/stdout /dev/stdin <<'EOF'
  function f() result(r)
    implicit none
    integer(16) :: r
    external side
    r = 42_16
    call side()
  end function
  EOF
  ```

- **Actual behavior:** The end of `_f` is:

  ```asm
  stp x16, x17, [x29, #-16]  ; saved 42
  ldp x0, x1, [x29, #-16]    ; moved ahead of side()
  ldp x29, x30, [sp, #16]
  add sp, sp, #32
  b _side
  ```

  The required `bl _side` has become a tail branch. Whatever `side` leaves in `x0:x1` becomes `f`'s result. At `-O0`, the compiler correctly emits `bl _side`, then reloads 42, then returns.
- **Intended behavior:** The i128 result reload is real post-call computation and must block tail-call conversion. `_f` must call `side`, reload 42 into `x0:x1`, and return to its own caller.
- **Consequence:** O1 and higher silently change the result of an i128-returning function merely because its last statement is a side-effecting subroutine call. The same opcode-only matching is unsafe for other legitimate post-call loads, including complex result marshalling and spill reloads.
- **Confidence:** Certain; the source input deterministically emits the reordered assembly above.

### 2. Tail calls tear down the frame containing overflow arguments before the callee reads them

- **Severity:** High
- **Source:** `src/codegen/arm64/isel.rs:338-340` reserves outgoing space and `src/codegen/arm64/isel.rs:397-399`, `2931-2947` store overflow arguments relative to the current `sp`. `src/codegen/arm64/mir.rs:489-500` makes that area part of the caller's frame. `src/codegen/arm64/tailcall.rs:94-125` has no overflow-argument guard and places the epilogue before the replacement `B`.
- **Reproducer:**

  ```sh
  target/debug/afs -ffree-form --target arm64-macos -O1 -S \
    -o /dev/stdout /dev/stdin <<'EOF'
  subroutine wrap() bind(c, name="wrap")
    use iso_c_binding
    interface
      subroutine sink(a1,a2,a3,a4,a5,a6,a7,a8,a9) bind(c, name="sink")
        import :: c_int
        integer(c_int), value :: a1,a2,a3,a4,a5,a6,a7,a8,a9
      end subroutine
    end interface
    call sink(11,22,33,44,55,66,77,88,99)
  end subroutine
  EOF
  ```

- **Actual behavior:** The assembly stores 99 at the wrapper's current `[sp]`, restores all callee-saved registers and FP/LR, executes `add sp, sp, #96`, and only then executes `b _sink`.
- **Intended behavior:** At `_sink` entry, its ninth argument must be at its entry `sp`. The compiler must retain `bl`/`ret`, disable this transformation when outgoing stack space is used, or relocate the overflow arguments to the post-epilogue stack location.
- **Consequence:** Every optimized tail-position call with more than eight GP arguments, more than eight FP arguments, or another stack-classified argument reads unrelated memory for its overflow arguments. This is silent ABI corruption.
- **Confidence:** Certain; the store and the 96-byte SP adjustment are both explicit in the generated assembly. A local Clang Apple-target compile of the equivalent C wrapper keeps `str w8, [sp]`, `bl _sink`, then the epilogue.

### 3. Optimized mixed scalar/i128 parameter receipt overwrites the incoming i128 pair

- **Severity:** High
- **Source:** `src/codegen/arm64/isel.rs:118-147` emits incoming receipts in source order, so narrow receipts precede the later i128 `StpOffset`. The allocator may place those narrow vregs in other incoming argument registers (`src/codegen/arm64/linearscan.rs:33-38`). `parallelize_entry_arg_moves` at `src/codegen/arm64/linearscan.rs:1241-1288` flushes the narrow move group before the intervening store and does not model still-unreceived argument registers.
- **Reproducer:**

  ```sh
  target/debug/afs -ffree-form --target arm64-macos -O1 -S \
    -o /dev/stdout /dev/stdin <<'EOF'
  function probe(a,b,c,d,w) result(r) bind(c,name="probe")
    use iso_c_binding
    integer(c_int), value :: a,b,c,d
    integer(16), value :: w
    integer(16) :: r
    r = w
  end function
  EOF
  ```

- **Actual behavior:** Apple places `a..d` in `w0..w3` and `w` in `x4:x5`. Armfortas emits:

  ```asm
  mov w7, w0
  mov w6, w1
  mov w5, w2               ; destroys incoming w high limb in x5
  mov w4, w3               ; destroys incoming w low limb in x4
  stp x4, x5, [x29, #-16]
  ldp x0, x1, [x29, #-16]
  ret
  ```

  The function returns `d + (c << 64)`, not `w`. The same source at `-O0` saves `x4:x5` before reusing scratch registers and is correct.
- **Intended behavior:** Save all fixed incoming values before assigning any destination that aliases an as-yet-unreceived argument register, or resolve scalar and pair receipts as one parallel-copy problem.
- **Consequence:** O1+ breaks both C interoperability and internal calls for mixed signatures whose i128 pair follows enough scalar arguments to overlap allocator-chosen destinations.
- **Confidence:** Certain. `clang -target arm64-apple-macos -O1` for `__int128 probe(int,int,int,int,__int128 w)` returns the untouched `x4:x5`, confirming the expected Apple register assignment.

### 4. CSEL fusion keeps stale NZCV across i128 arithmetic

- **Severity:** High
- **Source:** `compute_csel_fusion` at `src/codegen/arm64/isel.rs:3542-3587` only invalidates pending flags for another compare or a call. It does not recognize i128 `IAdd`, `ISub`, or `INeg` as flag-clobbering. Those operations emit `AddsReg`/`SubsReg` at `src/codegen/arm64/isel.rs:2951-3010`. Wide selects then consume the supposedly preserved flags at `src/codegen/arm64/isel.rs:738-796`.
- **Reproducer:**

  ```sh
  target/debug/afs -ffree-form --target arm64-macos -O1 -S \
    -o /dev/stdout /dev/stdin <<'EOF'
  integer(16) function f(x,y,a,b) result(r) bind(c, name="f")
    use iso_c_binding
    integer(c_int64_t), value :: x,y
    integer(16), value :: a,b
    logical :: cond
    integer(16) :: t,u
    cond = x < y
    t = a + b
    u = a - b
    r = merge(t,u,cond)
  end function
  EOF
  ```

- **Actual behavior:** Optimized IR is `icmp lt x,y; iadd a,b; isub a,b; select`. Assembly emits `cmp x7,x6`, but then `adds` for the addition and `subs` for the subtraction overwrite NZCV. The final pair of `csel ..., lt` reads the low-limb subtraction flags, not the `x < y` flags.
- **Intended behavior:** Materialize the boolean before any flag-setting instruction, re-compare it at the select, or invalidate fusion whenever an intervening MIR expansion can write NZCV.
- **Consequence:** For `x=0, y=1, a=5, b=3`, the intended result is `a+b = 8`; the emitted `subs 5,3` clears LT, so the function selects `a-b = 2`. Results depend on unrelated arithmetic between the comparison and `MERGE`.
- **Confidence:** Certain; instruction ordering and the final NZCV producer are explicit in the emitted assembly.

### 5. Legal O3 WHERE comparisons silently lower to `nop`, leaving the mask undefined

- **Severity:** High
- **Source:** `src/opt/vec_analysis.rs:1139-1142`, `1604-1618` accepts every scalar comparison operator and advertises NEON i64 comparisons. ARM selection only implements i32 Eq/Lt/Le/Gt/Ge at `src/codegen/arm64/isel.rs:1949-1971`; `Ne` and all `<2 x i64>` cases fall through to `ArmOpcode::Nop` while still claiming to define the mask. `VSelect` then consumes that value at `src/codegen/arm64/isel.rs:1866-1887`.
- **Reproducer:**

  ```sh
  target/debug/afs -ffree-form --target arm64-macos -O3 --emit-ir \
    -o /dev/stdout /dev/stdin <<'EOF'
  program p
    integer :: a(4), b(4)
    a=[0,1,0,2]
    b=-1
    where (a /= 0) b=1
    print *, b
  end program
  EOF
  ```

  Re-run with `-S` instead of `--emit-ir` to see the machine output.
- **Actual behavior:** IR contains `vicmp Ne ... : <4 x i32>` feeding `vselect`. Assembly for that body is:

  ```asm
  ldr q13, [x24, #0]
  ldr q12, [x24, #0]
  nop                         ; alleged mask definition
  mov.16b v10, v11            ; reads an unwritten mask register
  bsl.16b v10, v14, v12
  str q10, [x24, #0]
  ```

- **Intended behavior:** Implement `Ne` (for example, equality followed by mask inversion), or reject this vector plan and retain the scalar loop. The expected printed array is `-1 1 -1 1`.
- **Consequence:** A valid O3 `WHERE` writes lanes according to stale SIMD-register contents. Output is silently wrong and may vary with surrounding allocation. A focused `<2 x i64>` `WHERE (a > 0)` reproduces the same `nop` because no i64 `VICmp` opcode is selected.
- **Confidence:** Certain; the unsupported vector IR and undefined-mask assembly are both reproducible from the source above.

### 6. Large-offset i128 stores overwrite their low limb with the frame offset

- **Severity:** High
- **Source:** i128 lowering supplies `x16:x17` to `StpOffset` at `src/codegen/arm64/isel.rs:650-660`, `2853-2870`. For offsets beyond the immediate forms, `src/codegen/arm64/emit.rs:151-165`, `760-779` also hard-codes `x16` as the address-immediate scratch and only afterwards stores the original pair.
- **Reproducer:**

  ```sh
  target/debug/afs -ffree-form --target arm64-macos -O0 -S \
    -o /dev/stdout /dev/stdin <<'EOF'
  function large() result(r) bind(c, name="large")
    use iso_c_binding
    integer(c_int64_t) :: pad(600)
    integer(16) :: r
    pad(1)=7
    r=123456789012345678901_16
  end function
  EOF
  ```

- **Actual behavior:** After materializing the intended low limb in `x16` and high limb 6 in `x17`, the emitter produces:

  ```asm
  movz x16, #27701
  movk x16, #12086, lsl #16
  movk x16, #40833, lsl #32
  movk x16, #45390, lsl #48
  movz x17, #6
  movz x16, #4848          ; destroys the value limb
  sub x9, x29, x16
  str x16, [x9]            ; stores 4848
  str x17, [x9, #8]
  ```

- **Intended behavior:** Address synthesis must use a register that cannot alias either stored limb, or synthesize the address before loading/materializing `x16:x17`.
- **Consequence:** i128 constants, arithmetic results, copies, and returns backed by a slot more than 4095 bytes from FP acquire the slot offset as their low 64 bits. This is deterministic silent data corruption in otherwise ordinary large-frame functions.
- **Confidence:** Certain; the value register is visibly overwritten immediately before the store.

### 7. Accepted complex `VALUE` arguments are not implemented as Apple HFAs

- **Severity:** High
- **Source:** Arrays representing complex values fall into the generic GP classifier at `src/codegen/arm64/abi.rs:138-149`. Incoming setup only creates a wide parameter slot for i128 at `src/codegen/arm64/isel.rs:99-110`, and outgoing setup only special-cases i128 at `src/codegen/arm64/isel.rs:343-382`. Meanwhile `src/codegen/arm64/isel.rs:2823-2838` explicitly identifies `[f64 x 2]` and `[f32 x 2]` as complex ABI pairs, but only return handling uses those helpers.
- **Reproducer, complex(8):**

  ```sh
  target/debug/afs -ffree-form --target arm64-macos -O0 -S \
    -o /dev/stdout /dev/stdin <<'EOF'
  program p
    use iso_c_binding
    interface
      function take(z) result(r) bind(c, name="take")
        import :: c_double, c_double_complex
        complex(c_double_complex), value :: z
        real(c_double) :: r
      end function
    end interface
    complex(c_double_complex) :: z
    real(c_double) :: r
    z=cmplx(1.0_c_double,2.0_c_double,kind=c_double)
    r=take(z)
  end program
  EOF
  ```

  The wrong-register complex(4) case is independently reproducible with:

  ```sh
  target/debug/afs -ffree-form --target arm64-macos -O0 -S \
    -o /dev/stdout /dev/stdin <<'EOF'
  program p
    use iso_c_binding
    interface
      function take4(z) result(r) bind(c, name="take4")
        import :: c_float, c_float_complex
        complex(c_float_complex), value :: z
        real(c_float) :: r
      end function
    end interface
    complex(c_float_complex) :: z
    real(c_float) :: r
    z=cmplx(1.0_c_float,2.0_c_float,kind=c_float)
    r=take4(z)
  end program
  EOF
  ```

- **Actual behavior:** The complex(8) call ICEs at `src/codegen/arm64/isel.rs:612` with `isel: unmapped IR value ...`: the value was assigned a wide stack slot, but call selection asks for a nonexistent vreg. The corresponding complex(4) program compiles, but loads the packed eight bytes into `x0` and calls `_take4`.
- **Intended behavior:** Under the Apple ARM64 ABI, complex(4) is passed in `s0:s1` and complex(8) in `d0:d1`, consuming two FP/SIMD slots. A local Clang Apple-target callee for `_Complex float` reads `s0/s1`; one for `_Complex double` reads `d0/d1`.
- **Consequence:** C interoperability with a complex `VALUE` dummy either fails compilation (double complex) or silently passes the argument in the wrong register bank (single complex). The frontend accepts both declarations, so this is not an intentional diagnostic gate.
- **Confidence:** Certain; both the ICE and the complex(4) `mov x0,...; bl _take4` sequence were reproduced locally.

### 8. O2 contracts separate multiply/add operations even though value-changing fast math is Ofast-only

- **Severity:** Medium
- **Source:** `src/codegen/arm64/mod.rs:35-40` runs the peephole at every O2-or-higher level. `src/codegen/arm64/peephole.rs:60-71`, `153-215` unconditionally replaces `FMul` plus `FAdd` with `FMAdd`. The declared optimization policy at `src/opt/pipeline.rs:107-112` reserves value-changing floating-point transformations for `Ofast`.
- **Reproducer:**

  ```sh
  for O in 1 2 fast; do
    echo "-O$O"
    target/debug/afs -ffree-form --target arm64-macos -O$O -S \
      -o /dev/stdout /dev/stdin <<'EOF' | rg 'fmul|fadd|fmadd'
  real(c_double) function muladd(a,b,c) result(r) bind(c, name="muladd")
    use iso_c_binding
    real(c_double), value :: a,b,c
    r=a*b+c
  end function
  EOF
  done
  ```

- **Actual behavior:** O1 emits `fmul` followed by `fadd`; O2, O3, and Ofast all emit one `fmadd`.
- **Intended behavior:** O2/O3 must preserve the separately rounded operations under the compiler's stated policy; contraction belongs under Ofast or an explicit FP-contract option.
- **Consequence:** For exactly representable inputs `a=1+2^-27`, `b=1-2^-27`, `c=-1`, separate operations round the product to 1 and return +0, while `fmadd` returns exactly `-2^-54`. Exception flags and directed-rounding behavior can differ as well. This violates the documented cross-level value-equivalence invariant outside Ofast.
- **Confidence:** Certain for the optimization-level discrepancy and the stated numerical counterexample.

## Unconfirmed concerns

### V128 ABI paths are internally inconsistent, but no frontend vector signature was found

`classify_abi_arg` returns `AbiArgLoc::V128` (`src/codegen/arm64/abi.rs:123-136`), but incoming selection has no `(RegClass::V128, AbiArgLoc::V128)` arm (`src/codegen/arm64/isel.rs:118-213`) and outgoing selection has none at `src/codegen/arm64/isel.rs:382-405`. Call-result capture uses `fmov d0` for V128 (`src/codegen/arm64/isel.rs:524-538`), and function return falls through to the GP return path (`src/codegen/arm64/isel.rs:2385-2398`). Direct vector-signature IR would therefore panic or truncate, but current Fortran lowering appears not to expose vector types across function boundaries.

### V128 values cannot safely use the nominal FP callee-saved pool across a call

Linear scan treats V128 as FP (`src/codegen/arm64/linearscan.rs:279-315`) and can put a call-crossing value in `v8-v15`. The Apple/AAPCS rule preserves only the low 64 bits of those registers; full 128-bit caller values need a Q-width save. Callee-save insertion allocates eight-byte slots (`src/codegen/arm64/linearscan.rs:982-986`) and emits D-width saves. Split bridges also lose the V128 class and use FP-width loads/stores. The vectorizer currently rejects loops containing calls, so a frontend-reachable source reproducer was not confirmed.

### Tail-call frame-pointer taint does not flow across blocks or through all pointer operations

`has_frame_derived_arg` is invoked independently on each candidate block (`src/codegen/arm64/tailcall.rs:94-103`, `149-263`). A frame address created in a predecessor can arrive in a tail block without taint, and propagation omits operations such as `CselReg`, `SubReg`, and `OrrReg`. A deterministic MIR shape is unsafe, but a minimal source that survives lowering into that exact cross-block allocation was not established during this review.

### Narrow signed ABI values rely on incidental canonicalization

`IntTrunc` is only a 32-bit move (`src/codegen/arm64/isel.rs:1729-1738`), while call/return moves do not insert `sxtb`/`sxth`. Apple callers canonicalize signed byte/half arguments, and ordinary memory loads in armfortas use `ldrsb`/`ldrsh`, which hides this for common sources. A direct truncation feeding a C call can be noncanonical, but the reviewed Fortran ways to create an out-of-range narrow value were either canonicalized through memory or had questionable source-level overflow semantics, so this remains a backend concern rather than a confirmed Fortran discrepancy here.

## Maintainability observations

- Liveness treats operand zero as a use even when `inst.def` identifies it as a pure destination (`src/codegen/arm64/liveness.rs:220-233`, `269-282`). `src/codegen/arm64/linearscan.rs:210-222` explicitly documents retaining this known inflation because other allocation behavior depends on it. This makes values appear live before their definitions, overuses callee-saved registers, and makes correctness depend on an acknowledged analysis defect.
- Unsupported vector shapes often become a `Nop` with a live `def` instead of a diagnostic or a legality failure (`src/codegen/arm64/isel.rs:1783-1819`, `1825-1835`, `1915-1971`, `2091-2107`). The confirmed WHERE bug is one instance of a systemic silent-failure mode.
- Tail-call restore recognition is opcode-only. Callee-save insertion already knows exact save slots and registers; carrying that metadata would avoid trying to infer restores from generic load opcodes later.
- Scratch-register contracts live in comments and duplicated constants. The large-frame bug arose because the emitter could not know that its `x16` scratch aliases a fixed-value operand. Explicit pseudo-op constraints or a late scratch allocator would make this class of error harder to reintroduce.
- `ISelCtx::lookup_block` silently maps an unknown IR block to machine entry (`src/codegen/arm64/isel.rs:624-627`). A malformed or incompletely updated CFG can therefore become a wrong branch rather than a localized backend failure.

## Test gaps

- The 125 focused ARM64 unit tests all pass, but tail-call tests have no outgoing stack-argument case and no source-level i128/complex return load after a side-effect call.
- `parallelize_entry_arg_moves` tests cover register swaps and intervening stores, but not a still-unreceived fixed register pair following scalar receipts.
- `emit_large_negative_pair_offsets_use_scratch_addressing` uses offset -544 and pair `x0:x1`; it never reaches the greater-than-4095 immediate synthesis that aliases i128's `x16` limb.
- Vector ABI tests stop at `classify_abi_arg`; they do not run incoming, outgoing, or return selection. WHERE vector tests do not cover `Ne` or i64 comparisons on ARM64.
- Complex C-interoperability coverage exercises complex returns, not complex `VALUE` arguments. Host-gated differential tests on this Linux machine select the x86 backend and cannot expose Apple ARM64 register classes.
- Peephole tests assert that FMA fusion happens, but do not assert that it is gated by the floating-point policy or compare O2 output against a rounding-sensitive case.
- The liveness successor builder only records `B` and `BCond` (`src/codegen/arm64/liveness.rs:173-203`), despite MIR and relaxation supporting `Cbz/Cbnz/Tbz/Tbnz`. No current pre-liveness producer was found, but a future peephole introducing those branches would make CFG liveness incomplete.

## Checks with no confirmed discrepancy

- Generated scalar and vector assembly used in focused probes was accepted by `llvm-mc -triple arm64-apple-macos`.
- Piping a focused compiler output through `target/debug/afs-as - -o -` produced a Mach-O 64-bit arm64 object; `llvm-objdump --macho --syms -` showed the expected external `_add1` symbol.
- Conditional-branch inversion and range-expansion unit tests passed, and targeted inspection found no source-level branch-relaxation failure.
- Basic 16-byte SP alignment, large-frame probing, ordinary GP/FP callee saves, Mach-O underscore prefixing, and constant-pool relocation forms had no confirmed discrepancy in the inspected examples.
