# Brutal audit — optimizer pipeline (consolidated)

Two independent audits performed against the seven optimizer passes shipped on
the `optimizer` branch (`pass.rs`, `pipeline.rs`, `util.rs`, `const_fold.rs`,
`const_prop.rs`, `dce.rs`, `cse.rs`, `strength_reduce.rs`, `licm.rs`):

1. **Manual audit** by primary author: read every line, wrote adversarial unit
   tests in `src/opt/audit_tests.rs`, compiled real Fortran reproducers.
2. **Background brutal-auditor agent**: independent code review with no
   knowledge of the manual findings beyond M-1 and M-7. Raw report archived at
   `.docs/audits/optimizer-audit-agent-raw.txt`.

The agent identified **substantially more findings than the manual pass**.
This document is the union, deduplicated, and verified by re-reading the code
and running the new tests.

## Summary

| Severity   | Count | Items |
|------------|-------|-------|
| **Critical** | **3** | M-1 (live miscompile), C-A, C-C |
| Major        | 6     | M-7, M-B, M-C, M-D, M-E, M-F |
| Medium       | 6     | Med-2, Med-3, Med-5, Med-6, plus Med-1, M-A4 |
| Minor        | ~5    | code smells, perf, stale comments |
| Surprises    | 3     | look wrong, are correct (S-1, S-2, S-3) |

## CRITICAL bugs (ship-blockers — produce silently wrong code today)

### M-1 — `const_fold::IntToFloat` does not round through f32 — LIVE FORTRAN MISCOMPILE

**File:** `src/opt/const_fold.rs:291–297`

```rust
InstKind::IntToFloat(a, fw) => {
    if let Some(Const::Int(av, w)) = get(a) {
        let signed = sext(av, w.bits());
        return Some(InstKind::ConstFloat(signed as f64, *fw));
    }
    None
}
```

For an `f32` destination, the result is stored as the unrounded `f64` value
of the integer. Downstream FCmp/FAdd/FloatToInt folds use the unrounded f64,
diverging from runtime. **This produces wrong output today on real Fortran.**

**Live reproducer** (`/tmp/audit_m1_real.f90`):
```fortran
program p
    real(4) :: r
    r = 1.0_4 + real(16777217, 4)
    print *, r
end program
```
- `-O0`: `1.6777216E7` (correct)
- `-O2`: `1.6777218E7` (**WRONG**)

**Root cause path:** `lower.rs:548–557` lowers `real(N, 4)` to
`int_to_float(N, F32)`. `lower.rs:3470–3486` then promotes mixed-mode `iadd
+ const(int)` into `IntToFloat → FAdd`. `const_fold` folds the chain using
the unrounded f64.

**Failing tests:**
- `audit_const_fold_int_to_f32_must_round` — direct
- `audit_int_to_f32_then_fsub_wrong_answer_today` — chain demonstration

**Fix:** at the IntToFloat arm, round through destination width:
```rust
let v = match fw {
    FloatWidth::F32 => signed as f32 as f64,
    FloatWidth::F64 => signed as f64,
};
```
The fix is one line — `FloatExtend`/`FloatTrunc` and `fold_float_bin` already
do the equivalent correctly.

---

### C-A — `strength_reduce` Identity rewrite has a `continue` that swallows `changed`

**File:** `src/opt/strength_reduce.rs:307–322`

```rust
Rewrite::Identity { pass_through } => {
    substitute_uses(func, old_id, pass_through);  // already mutated function
    let placeholder = match ty {
        IrType::Int(w) => InstKind::ConstInt(0, w),
        IrType::Bool   => InstKind::ConstBool(false),
        _ => continue,                              // ← swallows `changed = true`
    };
    func.blocks[bi].insts[ii].kind = placeholder;
}
changed = true;  // ← never reached on the `continue` branch
```

**Today** this branch is unreachable in practice because `rewrite_for` only
emits `Identity` for integer ops on integer-typed instructions. But the
arrangement is a footgun: `substitute_uses` has *already mutated the
function* before the type check, and the `continue` skips both the
placeholder write **and** the `changed = true` flag. The pass returns
"no change" while having silently rewritten every use across the function.

The moment anyone adds an `fmul x, 1.0` → identity rewrite under fast-math,
or a `getelementptr x, 0` → identity, this fires and corrupts the IR
silently.

**Fix:**
1. Set `changed = true` *before* the placeholder branch.
2. Either restrict `rewrite_for` to int/bool with an `assert!`, or drop the
   placeholder machinery entirely (substitute already rewired the uses;
   leave the original instruction alone — DCE next round).

---

### C-C — `const_fold::Select` reemits constant with the SOURCE width, not the Select's declared width

**File:** `src/opt/const_fold.rs:354–369`

```rust
InstKind::Select(c, t, f) => {
    if let Some(Const::Bool(cv)) = get(c) {
        let chosen = if cv { *t } else { *f };
        if let Some(k) = consts.get(&chosen) {
            return Some(match *k {
                Const::Int(v, w)   => InstKind::ConstInt(v, w),    // w is from chosen, not inst.ty
                Const::Float(v, w) => InstKind::ConstFloat(v, w),  // same
                Const::Bool(b)     => InstKind::ConstBool(b),
            });
        }
    }
    None
}
```

The Select instruction's declared type is `inst.ty`, but the rewrite reuses
`w` from the chosen branch's constant record. If those differ — which can
happen via IntExtend/IntTrunc/FloatTrunc chains — the new `inst.kind`
embeds a different width than `inst.ty`.

The verifier's `check_type_consistency` only checks `is_int()` / `is_float()`
on operand types, not the kind's embedded width vs `inst.ty`. So the
mismatch sneaks through. Downstream CSE keys on `inst.ty`, while
`strength_reduce::collect_int_consts` reads the kind — they diverge.

**Fix:** use the destination type, with renormalization:
```rust
let chosen_const = consts.get(&chosen)?;
Some(match (&inst.ty, chosen_const) {
    (IrType::Int(w),   Const::Int(v, _))   => InstKind::ConstInt(norm(*v, *w), *w),
    (IrType::Float(w), Const::Float(v, _)) => InstKind::ConstFloat(round_for_width(*v, *w), *w),
    (IrType::Bool,     Const::Bool(b))     => InstKind::ConstBool(*b),
    _ => return None,
})
```

## MAJOR bugs (correctness, observable on real Fortran in some configurations)

### M-7 — `find_natural_loops` mis-computes body for self-loops

**File:** `src/opt/util.rs:174–185`

When `latch == header`, walking back through `preds(header)` adds the
preheader to the loop body, then `find_preheader` rejects the loop entirely.
LICM is silently disabled on self-loops.

**Failing test:** `audit_licm_dormant_with_alloca_load`

**Fix:** filter out latches that are the header from the BFS seed:
```rust
let mut stack: Vec<BlockId> = latches.iter()
    .filter(|&&l| l != header)
    .copied()
    .collect();
```

---

### M-B — `const_fold::Shl` out-of-range count returns 0; AArch64 returns identity

**File:** `src/opt/const_fold.rs:212–228`

The comment claims AArch64 returns 0 for shift count ≥ width. **This is
wrong.** AArch64 `LSL` masks the count to `bits-1`, so `lsl w0, w1, #32`
produces `w1`, not 0. `lsl x0, x1, #64` is identity.

This affects Fortran `ISHFT` lowering: the lowerer (per `src/ir/lower.rs:671`)
computes both `Shl(a, n)` and `LShr(a, -n)` and selects on the sign of `n`.
When `n` is a known constant ≥ width, const_fold returns 0 for the dead arm
— but anything that later inspects that constant (CSE, strength_reduce) sees
a value the runtime would never produce. Worse, if the lowering ever changes
to NOT use a guarding select, real code miscompiles.

`LShr` (line 229–242) has the same wrong return-0 branch.

**Fix:** for `bv < 0 || bv >= bits`, **return None** (leave the instruction
alone — the safest option, defers semantics to codegen). Do not try to
emulate AArch64 masking inside the fold; codegen needs to be the
single source of truth.

---

### M-C — `const_fold::Shl/LShr/AShr` with negative shift count emits 0 (also wrong)

**File:** `src/opt/const_fold.rs:212–258`

`bv as u32` casts a negative i64 to a huge positive u32. `(0..bits).contains(&bv)`
is false for negative, so the out-of-range return-0 path fires. AArch64 would
mask the negative count to its low log2(width) bits, producing a non-zero result.

**Fix:** same as M-B — bail (`return None`) for `bv < 0`.

---

### M-D — `const_fold::ICmp` reads operand b's value with operand a's width

**File:** `src/opt/const_fold.rs:149–164`

```rust
InstKind::ICmp(op, a, b) => {
    if let (Some(Const::Int(av, w)), Some(Const::Int(bv, _))) = (get(a), get(b)) {
        let av = sext(av, w.bits());
        let bv = sext(bv, w.bits());   // ← uses w from a
```

If the two operands have different stored widths in their constant records
(possible after IntExtend/IntTrunc fold chains, or after a future GVN unifies
casts), `bv` is sign-extended from `a`'s width — interpreting `b`'s bit
pattern with the wrong width.

Today the verifier enforces operand types match in arithmetic, but it does
NOT enforce width equality (only `is_int()`). So the bug is currently dormant
but the verifier won't catch it once a future pass introduces width drift.

**Fix:** read `b`'s own width: `let (Some(Const::Int(av, aw)), Some(Const::Int(bv, bw))) = ...`,
then `sext(av, aw.bits())` and `sext(bv, bw.bits())`. Better: assert
`aw == bw` and bail otherwise.

---

### M-E — `const_fold::FCmp` doesn't round through f32 for f32 operands

**File:** `src/opt/const_fold.rs:165–178`

Reads `Const::Float(av, _)` ignoring the stored width. If either operand
came from a chain that should have been rounded through f32 (M-1 family),
the comparison sees the unrounded f64 and produces a result that diverges
from runtime.

**Fix:** ties to M-1. Either round both operands through f32 when the
operand type is f32, or fix M-1 at the source so f32 ConstFloat values are
always already rounded.

---

### M-F — `const_fold::FloatToInt` uses unrounded f64 for f32 source

**File:** `src/opt/const_fold.rs:298–319`

Same family as M-1/M-E. The bounds check and `truncd as i64` cast use the
stored f64 value, even when the source was a (buggy unrounded) f32.

**Fix:** consult operand width and round before the cast/check.

## MEDIUM issues

### Med-6 — LICM may hoist trap-prone operations out of guarding conditionals

**File:** `src/opt/licm.rs:54–65`

`is_hoist_candidate` excludes Load/Store/Alloca/Call/RuntimeCall but not
`IDiv`/`IMod`/`FDiv`/`FSqrt`/`FPow`. A loop-invariant `idiv x, y` inside a
guarding `if y /= 0` block could be hoisted to the preheader, where it
executes unconditionally — including when `y == 0`, causing SIGFPE that
the original code would never hit.

**Today** LICM is dormant on real Fortran (no mem2reg → loads block
everything), so this is latent. **Once mem2reg lands**, this becomes a
real correctness bug.

```fortran
do i = 1, 10
  if (b /= 0) then
    c = a / b      ! a, b loop-invariant; safe today only because of guard
  end if
end do
```

**Fix:** extend `is_hoist_candidate` to exclude trap-prone pure ops:
```rust
fn is_hoist_candidate(kind: &InstKind) -> bool {
    !matches!(
        kind,
        InstKind::Load(..) | InstKind::Store(..) | InstKind::Alloca(..)
        | InstKind::Call(..) | InstKind::RuntimeCall(..)
        | InstKind::ConstString(..) | InstKind::Undef(..)
        // Trap-prone: division and roots can SIGFPE / produce NaN under
        // operands the guarding code intended to skip. Don't speculate.
        | InstKind::IDiv(..) | InstKind::IMod(..)
        | InstKind::FDiv(..) | InstKind::FSqrt(..) | InstKind::FPow(..)
    )
}
```

---

### Med-2 — `const_fold::IDiv/IMod` overflow guard only covers true i64

**File:** `src/opt/const_fold.rs:108–128`

The `i64::MIN/-1` check only protects the i64 width. **However**, the
narrow-width cases (i8::MIN/-1, i32::MIN/-1) are saved by `norm()`'s
mask-then-sext, which correctly produces -128, -2147483648, etc., matching
ARM64 SDIV.

Audit test `audit_const_fold_idiv_i8_min_neg_one` confirms this works.
The agent and I both initially flagged this as a bug, then traced through
and demoted it — listed here for completeness, **not a bug**.

---

### Med-3 — `const_fold::PopCount/CLZ/CTZ` use source operand width, not `inst.ty`

**File:** `src/opt/const_fold.rs:259–288`

The result type of these intrinsics is taken from the source operand's
stored width, not from `inst.ty`. Today they always match because the
lowerer respects that, but if `inst.ty` ever diverges (e.g., a future
lowering choice for popcount returning i32 always), the fold produces
constants with the wrong width.

**Fix:** consult `inst.ty` for the output width.

---

### Med-5 / C-1 — DCE doesn't remove dead block parameters

**File:** `src/opt/dce.rs`

DCE only considers instruction results, not block parameters. A block param
consumed by no instruction or terminator inside the block stays alive,
which keeps its corresponding branch arg values alive in predecessors,
which keeps the defining instructions alive. Chains of dead-but-referenced
block args can block significant DCE.

**Test:** `audit_dce_does_not_remove_dead_block_param` (passes — documents
the limitation).

**Fix complexity:** moderate. Removing a block param requires updating every
predecessor's branch arg list. Defer until benchmarks show the cost.

---

### Med-1 — DCE keeps `Alloca` even when truly dead

**File:** `src/opt/dce.rs:43–53`

Conservatively side-effecting because the address might escape. Without
alias analysis we can't tell. Mem2reg/SROA will handle the common scalar
case.

## MINOR / code smells

- **Min-1**: `cse::LocalCse` reapplies `substitute_uses` in a loop — perf
  not correctness.
- **Min-2**: `licm::block_index` built once but commented "rebuild per
  iteration"; never rebuilt. Currently safe because LICM doesn't structurally
  mutate `func.blocks`. Footgun for maintainers.
- **Min-3**: `const_fold::Shl` sign-extends shift count then `bv as u32` —
  see M-C.
- **Min-4**: `cse::Key` ConstFloat aux xor with width is redundant with the
  `ty` field. Same for `cse::Key` ConstInt aux's `bits * 2^70` arithmetic
  (which truncates to 0 anyway in the i64 cast).
- **Min-5**: `prune_unreachable` was duplicated between `const_prop` and
  `dce` (now imported from `util`).
- **Min-6**: `pipeline::OptLevel::fast_math()` exists but no pass consults
  it. Will become live with float reassociation.

## SURPRISES (look wrong, are correct)

### S-1 — `const_fold::IDiv/IMod` `i64::MIN/-1` check appears narrow

The guard only covers true 64-bit overflow. For narrower widths, Rust's
`/` operation succeeds (since the i64 result fits), then `norm()`
correctly wraps to two's-complement at the declared width. Verified by
`audit_const_fold_idiv_i8_min_neg_one`. Agent and manual reviewer both
initially flagged this then ruled it out.

### S-2 — `const_fold` doesn't walk blocks in RPO

A single pass may miss folds across blocks if `func.blocks` isn't in
reverse-postorder. The fixpoint pass manager re-runs us, so eventually
every fold lands. Documented by `audit_const_fold_non_rpo_block_order`.
Performance issue, not correctness.

### S-3 — Strength reduce chained identities (reverse order)

Chains of Identity rewrites work because the apply phase processes them in
descending `(block_idx, inst_idx)` order, so each substitution sees the
up-to-date pass-through target. Verified by
`audit_strength_reduce_chained_identities`.

## Recommendations by priority

### Ship-blockers (fix before any -O2 code reaches `fortsh`):

1. **M-1** — live miscompile, one-line fix
2. **C-A** — strength_reduce silent continue (footgun)
3. **C-C** — Select width mismatch (latent type-punning)
4. **M-B** / **M-C** — Shl/LShr/AShr out-of-range (wrong on Fortran ISHFT)
5. **Med-6** — LICM trap hoisting (latent until mem2reg, but cheap to fix)

### Next priority:

6. **M-7** — LICM self-loops (missed optimization, narrow real-world impact)
7. **M-D** — ICmp width-from-a (latent until widths drift)
8. **M-E** / **M-F** — f32 precision in compares/casts (ties to M-1)

### Eventually:

9. **Med-3** — PopCount/CLZ/CTZ width
10. **Med-5** — DCE block param cleanup
11. Minors / code smells

### Re-audit gates:

- After fixes 1–5, re-run the full audit_tests suite.
- After mem2reg lands (Sprint 29 next chunk), re-audit LICM specifically —
  Med-6 and M-7 will become real-world reproducible.
