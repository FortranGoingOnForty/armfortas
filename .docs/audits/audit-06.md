# Audit 06 — x86_64 Fortran code-generator correctness

Date: 2026-07-09
Target reviewed: x86_64 SysV ELF
Implementation commit: `23857aa48f3bc0160303842488e8578acb487fb1`
Revision state: local working tree; no implementation, test, CI, manifest, submodule, or other report was edited

## Executive summary

The review found six verified discrepancies:

- three high-severity silent wrong-code defects at procedure boundaries;
- one high-severity compiler-added-argument defect affecting `OPTIONAL, VALUE`; and
- two medium-severity ABI failures, one an ICE for a valid complex argument and one an accepted-but-incompatible C descriptor convention.

The most urgent issue is the linear-scan allocator's handling of incoming ABI registers. It treats each register as occupied only at the instruction that copies it, rather than live from function entry until that copy. Optimized functions can therefore overwrite a later argument before receiving it. A four-integer function that should return `44` returns `22`; the analogous two-double function returns its first argument instead of its second.

No additional discrepancy was verified in two-address conversion, ordinary spill insertion, frame layout, call alignment, call-split bridges, the SSE2 instruction ceiling, the reviewed peepholes, or textual assembly determinism.

## Method

I reviewed the Rust implementation under `src/codegen/x86/`, the compiler-added argument lowering in `src/ir/lower/`, and relevant integration/unit tests. Evidence was limited to source text, emitted IR, emitted `-S` assembly, diagnostics, exit status, and normal program stdout. No raw object or executable content is quoted or used as evidence.

Local validation included:

- 74 x86 backend unit tests: all passed;
- 17 `calling_convention_runtime` tests: all passed;
- 7 `c_interop_differential` tests: all passed;
- the full textual ISA-ceiling gate: clean across 1,236 successful `-O3`/`-Ofast` compilations;
- the full textual x87 gate: clean across 1,236 successful `-O2`/`-Ofast` compilations;
- repeated `-S` compilation of `ar13_unswitch_deterministic.f90`: pairwise identical at `O0`, `O1`, `O2`, `O3`, `Os`, and `Ofast`;
- an O2 call-pressure program, which printed `r 62` and `ok`; and
- an O3 SSE2 integer-vector dot product, which printed `11440`.

The passing suites do not cover the failing shapes below.

## Verified discrepancies

### A06-01 — Incoming argument copies overwrite arguments not yet received

Severity: **High**

Source locations:

- `src/codegen/x86/isel.rs:75-145` — classifies parameters, then emits sequential physical-register-to-vreg receipts;
- `src/codegen/x86/liveness.rs:238-251` — a dead parameter receives a point interval at its copy;
- `src/codegen/x86/linearscan.rs:149-200` — fixed-register occupancy consists only of explicit instruction positions;
- `src/codegen/x86/linearscan.rs:260-287,374-403` — expired registers are returned to the end of the free list and another incoming argument register may be selected.

Focused reproduction:

```fortran
function pick4(a, b, c, d) result(r) bind(c, name="pick4")
  use iso_c_binding
  integer(c_int), value :: a, b, c, d
  integer(c_int) :: r
  r = d
end function
```

A C caller invokes `pick4(11, 22, 33, 44)`. At `-O2`, `-S` emits:

```asm
movl %edi, %eax
movl %esi, %ecx
movl %edx, %esi
movl %ecx, %edx
movl %edx, %eax
```

Actual behavior:

```text
-O0: 44
-O2: 22
```

The second receipt writes `b` into `%ecx` while `%ecx` still contains the not-yet-received `d`; the fourth receipt consequently reads `b`. A two-`real(c_double)` version emits `movsd %xmm0,%xmm1; movsd %xmm1,%xmm2; movsd %xmm2,%xmm0` and returns the first value instead of the second. The error reproduces at `O1`, `O2`, `O3`, `Os`, and `Ofast`; forcing the naive allocator avoids it.

Intended behavior: all incoming ABI registers must be modeled as live-ins until their values have been captured, or the entry receipts must be resolved as a safe parallel copy/precolored sequence.

Consequence: valid optimized procedures silently receive the wrong integer or floating-point arguments. `BIND(C)` is directly affected, and internal procedures using by-value arguments can be affected by the same entry sequence.

Confidence: **Certain** — reproduced in textual assembly and normal execution with both GP and XMM register files.

### A06-02 — A register-passed `COMPLEX(C_FLOAT_COMPLEX), VALUE` dummy causes an ICE

Severity: **Medium**

Source locations:

- `src/codegen/x86/isel.rs:79-115` — `ComplexF32` is classified as an XMM argument, then sent through the scalar-FP receive path;
- `src/codegen/x86/isel.rs:3035-3046` — the eight-byte complex value is represented by a `Gp64` vreg containing raw bits;
- `src/codegen/x86/isel.rs:3326-3352` — `fp_move`/`fp_size` accept only `IrType::Float` and panic for the complex array representation;
- `src/codegen/x86/isel.rs:2722-2733` — the inverse packed XMM-to-GP transfer already exists for complex returns.

Focused reproduction:

```fortran
function c4_id(z) result(r) bind(c, name="c4_id")
  use iso_c_binding
  complex(c_float_complex), value :: z
  complex(c_float_complex) :: r
  r = z
end function
```

Actual `-S` result:

```text
INTERNAL COMPILER ERROR
at src/codegen/x86/isel.rs:3329:18
isel: expected a float type, got Array(Float(F32), 2)
```

Intended behavior: receive the packed low 64 bits from `%xmm0` into the raw-bit GP vreg, mirroring `MovqXmmToGp` used for a complex-float return from a call.

Consequence: a valid and common C-interoperable complex-float callee cannot be compiled. Complex-float returns and complex-double values are covered elsewhere, which makes the missing incoming direction easy to miss.

Confidence: **Certain** — minimal source deterministically reaches the reported panic.

### A06-03 — Narrow C return values are consumed without extension

Severity: **High**

Source locations:

- `src/codegen/x86/isel.rs:2736-2758` — every one-register non-FP return is copied from its physical return register through `emit_phys_to_vreg`;
- `src/codegen/x86/isel.rs:3065-3070` — `Bool`, `I8`, and `I16` destinations are `Gp32`, so the copy reads all of `%eax` instead of extending `%al` or `%ax` according to the declared type.

Focused reproduction:

```fortran
function check_i8() result(r) bind(c, name="check_i8")
  use iso_c_binding
  interface
    function dirty_i8() result(v) bind(c, name="dirty_i8")
      import :: c_signed_char
      integer(c_signed_char) :: v
    end function
  end interface
  integer(c_int) :: r
  if (dirty_i8() < 0_c_signed_char) then
    r = 1
  else
    r = 0
  end if
end function
```

The conforming text-only helper returns signed byte `0x80` while deliberately leaving unrelated upper bits in `%eax`:

```asm
dirty_i8:
    movl $0x12345680, %eax
    ret
```

Armfortas emits the equivalent of:

```asm
call dirty_i8
movl %eax, %edx
cmpl $0, %edx
```

Actual output: `0`.
Intended output: `1`, because the declared result is signed `-128` and the caller must sign-extend `%al` before using it as the backend's canonical `Gp32` value.

The same defect applies to `I16`; `logical(c_bool)` requires zero-extension/canonicalization rather than consuming arbitrary upper bits.

Consequence: calls to conforming C or independently compiled functions can silently miscompare, branch on, print, or forward narrow results incorrectly.

Confidence: **Certain** — reproduced with a deliberately dirty but ABI-valid return register and normal execution.

### A06-04 — Signed narrow register arguments are not canonicalized at the call boundary

Severity: **High**

Source locations:

- `src/codegen/x86/isel.rs:1493-1531` — narrow bit operations execute as 32-bit operations;
- `src/codegen/x86/isel.rs:1755-1768` — `IntTrunc` is only a 32-bit move and does not restore signed `I8`/`I16` canonical form;
- `src/codegen/x86/isel.rs:2603-2610` — GP call setup copies the existing `Gp32` value directly to an argument register without type-directed sign/zero extension.

Focused reproduction:

```fortran
function check_narrow_arg() result(r) bind(c, name="check_narrow_arg")
  use iso_c_binding
  interface
    function promote_i8(x) result(v) bind(c, name="promote_i8")
      import :: c_signed_char, c_int
      integer(c_signed_char), value :: x
      integer(c_int) :: v
    end function
  end interface
  integer(c_int) :: r
  r = promote_i8(ibset(0_c_signed_char, 7))
end function
```

The C peer is:

```c
int promote_i8(signed char x) { return x; }
```

`ibset` produces the valid signed-byte bit pattern `0x80`. Armfortas passes `%edi = 128`; a Clang `-O2` callee is permitted to rely on the signed-extension contract and copies `%edi` to `%eax`.

Actual output: `128`.
Intended output: `-128`.

Intended behavior: canonicalize register arguments from their declared ABI type at the call boundary—`movsbl`/`movswl` for signed narrow integers and zero-extension for Boolean/unsigned-like values—rather than relying on every prior operation to preserve an informal vreg invariant.

Consequence: negative `integer(c_signed_char)`/short values produced by bit operations or truncation can be observed as positive by C callees or internal by-value callees.

Confidence: **High** — reproduced against local Clang assembly and normal execution; the exact failure depends on a peer that legitimately consumes the ABI extension rather than defensively re-extending the low byte.

### A06-05 — `OPTIONAL, VALUE` has no compiler-added presence state

Severity: **High**

This finding is target-independent lowering, but it directly concerns the compiler-added argument contract consumed by the x86 backend.

Source locations:

- `src/ir/lower/unit.rs:508-567` — a `VALUE` dummy gets only a scalar slot; optional tracking is installed only in the by-reference branch;
- `src/ir/lower/core.rs:18747-18759` — an omitted `VALUE` actual is replaced with a typed zero;
- `src/ir/lower/expr.rs:1508-1530` — `PRESENT` checks null only for by-reference locals and otherwise returns constant true.

Focused reproduction:

```fortran
program p
  call probe()
  call probe(0)
contains
  subroutine probe(x)
    integer, value, optional :: x
    if (present(x)) then
      print *, 'present', x
    else
      print *, 'absent'
    end if
  end subroutine
end program
```

Actual output:

```text
 present           0
 present           0
```

Intended output:

```text
 absent
 present           0
```

The emitted assembly materializes a constant true for `present(x)`. Zero cannot serve as an absence sentinel because it is a valid present value.

Intended behavior: add a separate presence flag/pointer compiler argument for optional by-value dummies and make callers, callees, procedure interfaces, and cross-TU metadata agree on it.

Consequence: `PRESENT` and any control flow based on it are silently wrong for every omitted `OPTIONAL, VALUE` dummy.

Confidence: **Certain** — reproduced at O0 in emitted assembly and normal program output.

### A06-06 — Accepted `BIND(C) CHARACTER(*)` uses the internal hidden-length ABI, not a C descriptor

Severity: **Medium**

Source locations:

- `src/ir/lower/core.rs:1452-1484` — assumed-length flags are collected without excluding `BIND(C)`;
- `src/ir/lower/unit.rs:954-974` — trailing i64 hidden-length parameters are appended to function signatures;
- `src/ir/lower/stmt.rs:4643-4673` — call sites append the same hidden lengths;
- `src/ir/lower/core.rs:57296-57335` — BIND(C) character actuals are lowered to raw data pointers.

Focused reproduction:

```fortran
function char_len(s) result(n) bind(c, name="char_len")
  use iso_c_binding
  character(kind=c_char, len=*), intent(in) :: s
  integer(c_int) :: n
  n = len(s)
end function
```

Armfortas emits this textual IR signature:

```text
func @char_len(%0: ptr<ptr<i8>>, %1: i64) -> i32
```

and the x86 function reads its length from `%rsi`. A local gfortran `-S` reference emits a one-argument descriptor ABI and reads the element length from `8(%rdi)`.

For a scalar C descriptor whose `elem_len` is `3`, with the otherwise non-argument `%rsi` preloaded to `777`:

```text
Armfortas callee: 777
descriptor reference callee: 3
```

Actual behavior: valid interoperable syntax is accepted but given armfortas's internal raw-pointer-plus-hidden-length convention.
Intended behavior: pass one `CFI_cdesc_t` pointer as required for the interoperable assumed-length form, or reject the declaration with the documented “C descriptors unsupported” diagnostic until that ABI exists.

Consequence: conforming C callers/callees disagree with generated code and can read bogus lengths or misinterpret descriptor memory. The current interop test passes because its C function accepts a raw pointer, receives explicit user lengths, and ignores extra trailing hidden arguments.

Confidence: **Certain** — reproduced in Armfortas IR/assembly, local reference assembly, and normal execution.

## Component review summary

| Area | Evidence and conclusion |
|---|---|
| SysV arguments | Scalar GP/XMM, independent register exhaustion, stack overflow slots, i128 pairs, and complex-double pairs are implemented. A06-01 breaks entry receipt ordering; A06-02 and A06-04 break complex-float and narrow-value cases. |
| SysV returns | i32/i64/i128, f32/f64, complex-float, and complex-double paths were inspected. A06-03 breaks narrow integer/Boolean call results. Aggregate by-value arguments and BIND(C) derived-type returns are explicitly rejected before this backend, so the classifier's aggregate/memory-return paths are not end-to-end exercised. |
| Compiler-added arguments | Internal trailing character lengths, hidden result descriptors, contained-procedure host references, procedure-closure slots, and their spill ordering have substantial passing runtime coverage. A06-05 lacks presence state for `OPTIONAL, VALUE`; A06-06 applies the internal character-length convention at a C boundary. |
| Instruction selection | Integer/FP arithmetic, comparisons including NaNs, fixed-register division/shifts, i128 carry chains, conversions, vector selection/reductions, and return moves were reviewed. Findings A06-02 through A06-04 are type/boundary selection defects. |
| Two-address conversion | The pass inserts the tied copy, swaps commutative operands when safe, and protects the non-commutative `def == rhs` case with a fresh temporary. Tie assertions and focused unit tests passed; no wrong-code discrepancy was verified. |
| Liveness | CFG successor discovery is adequate for the backend's explicit-jump block shapes, and call crossings are modeled. Entry physical-register live-ins are absent, causing A06-01. |
| Register assignment | Fixed explicit/implicit registers, call clobbers, callee-saved tracking, and deterministic ordering were reviewed. A06-01 is a precolored/live-in modeling failure rather than ordinary vreg interference. |
| Spills | Scalar, narrow, XMM, XMM128, split-call bridge, and callee-save slots were inspected. The call-pressure program and calling-convention suite passed; no additional spill-width or scratch-collision failure was verified. |
| Frames | Slots are aligned downward from `%rbp`, outgoing arguments occupy the bottom of a 16-byte-aligned frame, calls see aligned `%rsp`, large frames are probed, and returns restore from `%rbp`. No frame overlap/alignment discrepancy was verified. |
| Calls | Direct/indirect calls, exact `%al` XMM counts for external calls, stack-argument reservation, batched register setup, and return capture were inspected. Entry receipt, narrow canonicalization, and descriptor conventions account for the verified call defects. |
| SSE2 restriction | The MIR/emitter contains only legacy scalar SSE and packed SSE2 forms. The complete textual ceiling and x87 scripts were clean over 1,236 successful compilations each; selected O3 vector assembly used `pmuludq`, `pcmpgtd`, `pandn`, `pshufd`, and `movups`, with no SSE3+/AVX/x87 mnemonic. |
| Peepholes | Pre-regalloc compare-zero/test and self-move transforms, plus post-regalloc store forwarding, LEA folding, and XOR-zeroing, were reviewed. Flag-liveness guards protect compare/branch and add/adc chains; focused tests passed and no semantic discrepancy was verified. |
| Deterministic text | Function/block/constant order is vector-based; spill IDs, live intervals, split records, and callee-save sets receive explicit stable ordering before emission. Repeated representative `-S` output was identical at every optimization level. |

## Maintainability notes

1. **Physical-register hints are structurally inert.** Hints are created from an instruction that explicitly mentions the physical register (`src/codegen/x86/liveness.rs:267-297`), which also places that register inside the vreg's interval in fixed occupancy. The inclusive check at `src/codegen/x86/linearscan.rs:189-200,382-388` therefore rejects the hint it was meant to use. This adds moves and helped expose A06-01. Entry/call/return copies need first-class precolor or copy semantics rather than point occupancy plus hints.

2. **Captured regalloc MIR is not production regalloc MIR.** Production uses linear scan at every optimization level (`src/codegen/mod.rs:26-35,81-87`), while x86 `Stage::Regalloc` capture always runs the naive allocator (`src/testing.rs:542-560`) before separately asking production codegen for assembly. This can hide allocator defects such as A06-01 during stage triangulation.

3. **ABI packing is duplicated by direction.** Complex and narrow values have separate incoming, outgoing, and return cases. A type-directed marshal/unmarshal plan shared by caller and callee would make extension, packing, and register-pair symmetry reviewable in one place.

4. **The peephole flag table is conservative but semantically incomplete.** `Ucomiss/Ucomisd` and calls are treated as flag-transparent in `src/codegen/x86/peephole.rs:301-313`; from the optimizer's perspective they define/clobber flags. The current error suppresses opportunities rather than enabling an unsafe rewrite, but the table should describe actual effects before more flag-sensitive patterns are added.

## Coverage notes

1. `tests/abi_differential.rs:351-366` still unconditionally skips both Armfortas caller and callee legs, so its return, revert, mixed-aggregate, and alignment matrix is presently C/C-only.

2. `tests/c_interop_differential.rs` covers complex returns, not complex `VALUE` inputs. It also lacks dirty-upper-bit narrow returns and signed narrow register actuals.

3. Existing call-pressure tests keep all formal values live. Add optimized callees with unused/short leading GP and XMM formals; these are the minimal trigger for A06-01.

4. Add paired `call probe()` / `call probe(0)` coverage for every supported `OPTIONAL, VALUE` scalar category and across module/interface boundaries.

5. `tests/c_interop_differential.rs:377-429` documents a raw-pointer BIND(C) character convention. Its C fixture ignores the compiler's extra trailing hidden length, so it validates an internal convention rather than the F2018 descriptor ABI and masks A06-06.

6. `tests/determinism_sweep.rs` checks the broad corpus only at O2 and treats a failed first compilation as a skip. That can report determinism success while excluding deterministic compiler failures. The two audit-shaped programs get stronger eight-run coverage, but other optimization levels do not.

7. The ISA-ceiling and x87 scripts intentionally continue past compilation failures. Their clean 1,236-compilation result proves the ceiling for emitted text, not for sources that failed before emission.

8. By-value derived-type arguments and BIND(C) derived-type returns currently produce explicit frontend diagnostics. This is preferable to wrong ABI output, but it leaves the aggregate classifier and memory-return support without a real Fortran end-to-end consumer.

## Priority

1. Model incoming ABI registers as live-ins/parallel copies and add dead-leading-argument tests.
2. Canonicalize narrow values at both call and return boundaries.
3. Add presence state for `OPTIONAL, VALUE` and propagate it through interfaces and cross-TU metadata.
4. Add the missing packed complex-float receive.
5. Reject assumed-length BIND(C) character dummies until the C descriptor ABI is implemented.
