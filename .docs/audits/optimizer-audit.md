# Brutal audit: optimizer pipeline (passes 1–7)

Audit performed against the seven passes shipped on the `optimizer` branch:
`pass.rs`, `pipeline.rs`, `util.rs`, `const_fold.rs`, `const_prop.rs`, `dce.rs`,
`cse.rs`, `strength_reduce.rs`, `licm.rs`.

Methodology: read every line of every pass file, write adversarial unit tests
to pin down expected behavior, run them against current code, compare. All
findings below are backed by a failing or passing test in
`src/opt/audit_tests.rs`.

## Summary

| Severity | Count | Notes |
|---|---|---|
| **Critical** | **1** | **M-1 — live silent miscompile via mixed-mode `real(N, 4)` arithmetic** |
| Major | 1 | M-7 — LICM dormant on self-loops |
| Medium | ~9 | missed optimizations + verifier coverage gaps |
| Minor | ~3 | code smells |

## Critical bugs

### M-1 — `const_fold::IntToFloat` does not round through f32 (LIVE MISCOMPILE)

**Severity:** CRITICAL (silently produces wrong floating-point results)
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

For an `f32` destination, the result is stored as the unrounded `f64`
representation of the integer. Any downstream IR-level use of the value
that goes through another `const_fold` operation reads the unrounded
value, diverging from runtime.

**Repro 1** (unit, fails today):
`opt::audit_tests::audit_const_fold_int_to_f32_must_round` —
`IntToFloat(16777217:i32, F32)` folds to `ConstFloat(16777217.0, F32)`,
expected `ConstFloat(16777216.0, F32)`.

**Repro 2** (unit, fails today): `audit_int_to_f32_then_fsub_wrong_answer_today` —
chained `IntToFloat → FSub` folds to `1.0`, runtime gives `0.0`.

**Repro 3 (LIVE FORTRAN MISCOMPILE):** the following 4-line program
produces different output at `-O0` vs `-O2`:

```fortran
program p
    real(4) :: r
    r = 1.0_4 + real(16777217, 4)
    print *, r
end program
```

```
$ afs -O0 ... && ./prog
     1.6777216E7         ← correct (16777216, the f32 round of 16777217 stays
                           below the +1 ULP)
$ afs -O2 ... && ./prog
     1.6777218E7         ← WRONG (the f64-stored 16777217.0 propagates through
                           the FAdd fold and lands at 16777218)
```

The lowering at `src/ir/lower.rs:548–557` calls `b.int_to_float(arg, F32)`
for `real(N)` / `real(N, K)`. The mixed-mode promotion at
`src/ir/lower.rs:3470–3486` then chains the result into `FAdd`, and
`const_fold` uses the f64 value of the `IntToFloat` result directly in
`fold_float_bin`. The resulting `1.0 + 16777217.0 = 16777218.0` then
gets f32-rounded — but the rounding at the FAdd level doesn't undo the
fact that the input was already wrong by 1 ULP.

**Initial assessment was wrong:** I thought this required mem2reg to
trigger. It doesn't — every `real(N, kind)` in Fortran source today
goes through this exact path.

**Fix:** at the `IntToFloat` arm, round through the destination
precision before storing:

```rust
let v = match fw {
    FloatWidth::F32 => signed as f32 as f64,
    FloatWidth::F64 => signed as f64,
};
return Some(InstKind::ConstFloat(v, *fw));
```

The `FloatTrunc`/`FloatExtend` arms already do this correctly — verified
by `audit_const_fold_float_trunc_must_round`.

---

### M-7 — `find_natural_loops` mis-computes body for self-loops

**Severity:** MAJOR (silent missed optimization on self-loops)
**File:** `src/opt/util.rs:174–185`

```rust
let mut stack: Vec<BlockId> = latches.clone();
while let Some(b) = stack.pop() {
    if let Some(plist) = preds.get(&b) {
        for &p in plist {
            if p == header { continue; }
            if body.insert(p) {
                stack.push(p);
            }
        }
    }
}
```

When a loop has `latch == header` (a self-loop, where the header
unconditionally branches to itself or has a self-edge from a
conditional), `latches.clone()` puts `header` on the stack. Walking
`preds(header)` then enumerates the preheader and walks backward into
the function entry, pulling everything reachable into the loop body.

The downstream consequence: `find_preheader` looks for "the unique
predecessor of `header` that is not in `body`." When the body
incorrectly includes the preheader, this predicate fails and LICM
silently skips the loop.

**Repro:** `opt::audit_tests::audit_licm_dormant_with_alloca_load`
constructs a self-looping header with an invariant `const(1)` that
LICM should hoist. Currently the test fails because LICM skips the
loop entirely.

**Real-world impact today:** low — Fortran do-loops generate
header/body/latch/exit, where `latch != header`. WHERE/FORALL or
`do concurrent` may produce self-edges in some lowerings.

**Fix:** filter latches that are the header itself out of the BFS
seed:

```rust
let mut stack: Vec<BlockId> = latches.iter()
    .filter(|&&l| l != header)
    .copied()
    .collect();
```

Header and any latches stay in `body` (already inserted before the
walk); we just don't walk back through `preds(header)`.

## Medium issues (missed optimization or coverage gaps)

### C-1 — DCE does not remove dead block parameters

**File:** `src/opt/dce.rs`

DCE marks instruction results as live based on uses, but never
considers block parameters. A block param consumed by no instruction
or terminator inside the block (and never read by a successor's
branch arg) is dead but stays.

**Repro:** `opt::audit_tests::audit_dce_does_not_remove_dead_block_param`
documents the current behavior. (Test passes — it asserts the
limitation rather than the desired behavior.)

**Fix complexity:** moderate — removing a block param requires
updating every predecessor's branch arg list to drop the corresponding
slot. Defer until we see a benchmark hit, since dead block params are
rare in our lowering today.

---

### C-2 — LICM is mostly dormant pending mem2reg

**File:** `src/opt/licm.rs`

LICM correctly refuses to hoist `Load` (no alias analysis), and every
Fortran local lives in an alloca slot accessed via Load. So in real
Fortran source, virtually nothing in a loop body is provably invariant.

**Evidence:** `loop_sum.f90` IR at -O2 still has the `s + i` chain
intact in the body. The only LICM win observable today is when
`strength_reduce` synthesizes a constant inside the body which LICM
hoists out (then CSE/DCE collapse it).

**Fix complexity:** depends on a separate mem2reg/SROA pass (Sprint 29
"Memory Optimizations"). Not a bug in LICM itself; just notes that LICM
is paying the bare minimum until then.

---

### M-3 — Strength reduction misses negative-power-of-two multiplies

**File:** `src/opt/strength_reduce.rs`

`imul x, -2` could become `ineg (shl x, 1)`. `imul x, -4` →
`ineg (shl x, 2)`. Currently we only special-case `-1` and bail on
other negatives.

---

### M-4 — Strength reduction does not handle pow-of-two signed division

**File:** `src/opt/strength_reduce.rs`

`idiv x, 2^k` for signed `x` requires sign-bias before the shift:
`(x + ((x >> (bits-1)) >>> (bits-k))) >> k`. Three insns vs one
SDIV — still profitable, but documented as deferred.

---

### M-5 — Const-fold shift count ≥ width returns 0; ARM64 specifics

**File:** `src/opt/const_fold.rs:212–256`

For `Shl x, count` where `count >= width`, we return 0. ARM64 LSL
masks `count & (bits-1)` for the W register encoding. The two
behaviors disagree on `lsl w0, w1, #32` (and similar). We need to
make sure codegen agrees with the fold so that runtime and
compile-time give the same answer.

**Risk:** low — Fortran `ISHFT(x, count)` with count beyond the width
is implementation-defined per the standard, but we should at least be
self-consistent.

---

### M-6 — CSE is local-only

**File:** `src/opt/cse.rs`

We dedupe inside one block but never across dominating blocks. A
constant-loaded register followed by an `iadd` repeated in two
basic blocks of a straight-line function won't dedupe. GVN or global
CSE is the next step.

---

### M-7b — CSE never dedupes loads

**File:** `src/opt/cse.rs`

Two loads of the same address inside one block are left as separate
instructions. Without alias analysis we can't safely dedupe (an
intervening Store/Call could have written the slot). Unblocking this
needs a load-store-forwarding / alias analysis pass.

---

### M-8 — Const fold `Select` gives up on non-constant branches

**File:** `src/opt/const_fold.rs:354–369`

When the condition is a known constant, we fold to the chosen branch
**only if that branch is itself a constant**. If the chosen value is
a non-constant `ValueId`, we bail rather than rewrite uses.

**Why:** the const_fold pass has no `substitute_uses` infrastructure,
so it can only rewrite `inst.kind`, not retarget consumers. The
cleaner fix is to do this in const_prop (which already has
substitute machinery via util), or refactor const_fold to depend on
util's helpers.

---

### M-9 — Verifier type-checking is loose on bit widths

**File:** `src/ir/verify.rs:427–486`

`check_type_consistency` only verifies that integer operands are
*some* integer width (not the same width as the result), and
similarly for floats. An IR like `iadd %a:i8, %b:i64` declared as
`i32` would pass verification.

**Why this matters for the audit:** the verifier is the safety net we
rely on after every pass. If a pass produced a width-mismatched
rewrite, the verifier wouldn't catch it. None of the audited passes
appear to produce such rewrites (CSE only dedupes when the `ty` field
matches; strength_reduce preserves the operand width; const_fold
respects `inst.ty`), but the verifier should still tighten this up
to make future passes safer.

---

### M-10 — DCE keeps `Alloca` even when truly dead

**File:** `src/opt/dce.rs:43–53`

Allocas are conservatively side-effecting because their address might
escape via a future Store/Call. Without alias analysis we can't tell.

This is correct as a default but creates dead stack slots in
practice. Mem2reg/SROA will handle the common scalar case.

## Minor / code smells

### MN-1 — CSE constant-key encoding has misleading arithmetic

**File:** `src/opt/cse.rs:62–68`

```rust
InstKind::ConstInt(v, w) => mk(1, vec![], (*v as i128 + (w.bits() as i128) * (1i128 << 70)) as i64),
```

The `1i128 << 70` shift overflows the i64 cast and contributes 0 to
the aux. The encoding is only correct because the `Key` already
carries `ty: IrType`, which disambiguates widths. The arithmetic is
dead weight and should be replaced with a simple `*v`.

---

### MN-2 — `prune_unreachable` is duplicated between `const_prop` and `dce`

`const_prop::prune_unreachable` and `dce::prune_unreachable` (now via
`util::prune_unreachable`) used to be near-identical copies. The
util version is the canonical one — either pass that doesn't import
it should switch.

---

### MN-3 — Pipeline `fast_math()` predicate exists but is unused

**File:** `src/opt/pipeline.rs:61–63`

`OptLevel::fast_math()` returns `true` only for `Ofast`, but no pass
consults it. It will become live when we add float reassociation.
Document or annotate as `#[allow(dead_code)]` to avoid drift.

## Surprises (look wrong, are actually fine)

These came up during the audit and burned analyst time before being
ruled out. Documented so future audits don't re-litigate them.

### S-1 — `const_fold::IDiv` of i8::MIN by -1

Looks like it should overflow because i8::MIN / -1 doesn't fit in
i8. In practice, the i64 division produces a value that does fit in
i64, and `norm()` masks it back to 8 bits, yielding -128 — which
matches ARM64 SDIV semantics for i32 SDIV (and any narrow width).
Verified by `audit_const_fold_idiv_i8_min_neg_one`.

### S-2 — `strength_reduce` chained Identity rewrites

The pass turns `imul x, 1` into a Identity that retargets uses to
`x`, then turns the original instruction into `ConstInt(0)` as a
placeholder for DCE to remove. Chains of identities (e.g.,
`((x*1)*1)+0`) might look like they'd corrupt the chain because
substitutions happen across rewrites.

In practice the rewrites are sorted in reverse `(block_idx, inst_idx)`
order before applying, so the latest (highest-index) identity is
processed first. This means later substitutions execute before
earlier ones, so each substitution sees the up-to-date pass-through
target. Verified by `audit_strength_reduce_chained_identities` and
`audit_strength_reduce_mixed_shl_and_identity_in_block`.

### S-3 — `const_prop` dropping a CondBranch arm could break SSA dominance

Folding a CondBranch and dropping the false target can leave a merge
block whose now-only predecessor is a different block, potentially
breaking the dominance of values used in the merge. In practice this
is fine because (a) dominance is computed from the CFG, and (b) when
the false arm is unreachable, every value defined in it that was
used by a still-live merge block was a branch arg, and the branch arg
is dropped along with the arm. Verified by
`audit_const_prop_merge_after_drop`.
