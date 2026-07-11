//! Constant folding pass.
//!
//! Walks each function and replaces every instruction whose operands
//! are all compile-time constants with the corresponding `Const*`
//! instruction. The `ValueId` of each folded instruction is preserved
//! so that downstream uses see the same SSA name and don't need
//! rewriting.
//!
//! We deliberately keep this pass narrow:
//!
//!  * Only operands defined by `ConstInt` / `ConstFloat` / `ConstBool`
//!    in the same function are eligible.
//!  * Integer arithmetic uses two's-complement wrapping with the
//!    instruction's declared width — we mask down to 8/16/32/64 bits
//!    so that `i8 + i8` overflow matches what codegen would produce.
//!  * Integer division/modulo by zero is left alone — Fortran's
//!    behavior on divide-by-zero is implementation-defined, and the
//!    runtime / hardware should observe it the same way it would
//!    without optimization.
//!  * Float operations follow IEEE 754 semantics; division by zero
//!    yields well-defined ±inf or NaN and is folded.
//!
//! Constant *propagation* (replacing uses of a value that happens to
//! be a constant) is a separate pass; this one only rewrites the
//! defining instruction in place. The pair runs to fixpoint via the
//! pass manager, so propagation feeds folding which feeds propagation.

use super::pass::Pass;
use crate::ir::inst::*;
use crate::ir::types::{FloatWidth, IntWidth, IrType};
use std::collections::HashMap;

/// Compile-time constant value, normalized for folding.
#[derive(Debug, Clone, Copy)]
enum Const {
    Int(i128, IntWidth),
    Float(f64, FloatWidth),
    Bool(bool),
}

impl Const {
    /// Build a `Const` from a constant-producing instruction, putting
    /// the stored value into its **canonical** form for the declared
    /// width. Audit N-9 (root-cause fix for N-1/M-1/M-2/M-5):
    ///
    ///  * Integer values are sign-extended at their width. After this
    ///    call, `Const::Int(-1, I8)` and `Const::Int(255, I8)` both
    ///    land at the same stored value (`-1`), so downstream folds
    ///    can destructure `Const::Int(av, _)` without needing to
    ///    re-sext at the source width. This retires 12+ width-drift
    ///    hazards across IAdd/ISub/IMul/IDiv/IMod/BitAnd/... in one
    ///    move.
    ///  * Float values are rounded through their declared precision
    ///    (f32 values become exactly representable in f32 after
    ///    `v as f32 as f64`). This is belt-and-suspenders on top of
    ///    the producer-side guarantee from M-1.
    fn from_inst(kind: &InstKind) -> Option<Self> {
        match kind {
            InstKind::ConstInt(v, w) => Some(Const::Int(sext(*v, w.bits()), *w)),
            InstKind::ConstFloat(v, w) => Some(Const::Float(round_for_width(*v, *w), *w)),
            InstKind::ConstBool(b) => Some(Const::Bool(*b)),
            _ => None,
        }
    }
}

/// Sign-extend `v` from `bits` to a full i128. Used so that comparisons
/// and arithmetic on narrower integer types match hardware behavior.
fn sext(v: i128, bits: u32) -> i128 {
    if bits >= 128 {
        v
    } else {
        let shift = 128 - bits;
        (v << shift) >> shift
    }
}

/// Mask `v` down to `bits` low bits. The IR stores integers as i128 but
/// arithmetic must wrap at the declared width.
fn mask(v: i128, bits: u32) -> i128 {
    if bits >= 128 {
        v
    } else {
        v & ((1i128 << bits) - 1)
    }
}

/// Truncate-then-sign-extend a wide arithmetic result back to its
/// declared width, normalized to a sign-extended i128.
fn norm(v: i128, w: IntWidth) -> i128 {
    sext(mask(v, w.bits()), w.bits())
}

fn signed_min(w: IntWidth) -> i128 {
    match w {
        IntWidth::I8 => i8::MIN as i128,
        IntWidth::I16 => i16::MIN as i128,
        IntWidth::I32 => i32::MIN as i128,
        IntWidth::I64 => i64::MIN as i128,
        IntWidth::I128 => i128::MIN,
    }
}

/// Round an `f64`-stored value to the precision of its declared
/// `FloatWidth`. Audit B-3: used as defense in depth in `FCmp` and
/// `FloatToInt` so that those folds don't silently miscompute when
/// (some hypothetical future) producer breaks the M-1 invariant
/// (every `Const::Float(_, F32)` value is already f32-rounded).
fn round_for_width(v: f64, w: FloatWidth) -> f64 {
    match w {
        FloatWidth::F32 => v as f32 as f64,
        FloatWidth::F64 => v,
    }
}

/// Try to evaluate a single instruction given a map of known
/// constants. Returns `Some(new_kind)` if the instruction can be
/// replaced with a `Const*`.
#[allow(clippy::too_many_lines)]
fn try_fold(
    kind: &InstKind,
    ty: &IrType,
    consts: &HashMap<ValueId, Const>,
    fpenv_barrier: bool,
) -> Option<InstKind> {
    if fpenv_barrier && super::fpenv::is_rounding_dependent_fp(kind) {
        return None;
    }

    let get = |id: &ValueId| consts.get(id).copied();

    match kind {
        // Integer arithmetic ----------------------------------------------
        InstKind::IAdd(a, b) => {
            if let (Some(Const::Int(av, _)), Some(Const::Int(bv, _))) = (get(a), get(b)) {
                if let IrType::Int(w) = ty {
                    return Some(InstKind::ConstInt(norm(av.wrapping_add(bv), *w), *w));
                }
            }
            None
        }
        InstKind::ISub(a, b) => {
            if let (Some(Const::Int(av, _)), Some(Const::Int(bv, _))) = (get(a), get(b)) {
                if let IrType::Int(w) = ty {
                    return Some(InstKind::ConstInt(norm(av.wrapping_sub(bv), *w), *w));
                }
            }
            None
        }
        InstKind::IMul(a, b) => {
            if let (Some(Const::Int(av, _)), Some(Const::Int(bv, _))) = (get(a), get(b)) {
                if let IrType::Int(w) = ty {
                    return Some(InstKind::ConstInt(norm(av.wrapping_mul(bv), *w), *w));
                }
            }
            None
        }
        InstKind::IDiv(a, b) => {
            if let (Some(Const::Int(av, _)), Some(Const::Int(bv, _))) = (get(a), get(b)) {
                if let IrType::Int(w) = ty {
                    if bv == 0 {
                        return None;
                    } // leave divide-by-zero to runtime
                    if av == signed_min(*w) && bv == -1 {
                        return None;
                    }
                    return Some(InstKind::ConstInt(norm(av / bv, *w), *w));
                }
            }
            None
        }
        InstKind::IMod(a, b) => {
            if let (Some(Const::Int(av, _)), Some(Const::Int(bv, _))) = (get(a), get(b)) {
                if let IrType::Int(w) = ty {
                    if bv == 0 {
                        return None;
                    }
                    if av == signed_min(*w) && bv == -1 {
                        return None;
                    }
                    return Some(InstKind::ConstInt(norm(av % bv, *w), *w));
                }
            }
            None
        }
        InstKind::INeg(a) => {
            if let Some(Const::Int(av, _)) = get(a) {
                if let IrType::Int(w) = ty {
                    return Some(InstKind::ConstInt(norm(av.wrapping_neg(), *w), *w));
                }
            }
            None
        }

        // Float arithmetic ------------------------------------------------
        InstKind::FAdd(a, b) => fold_float_bin(get(a), get(b), ty, |x, y| x + y),
        InstKind::FSub(a, b) => fold_float_bin(get(a), get(b), ty, |x, y| x - y),
        InstKind::FMul(a, b) => fold_float_bin(get(a), get(b), ty, |x, y| x * y),
        InstKind::FDiv(a, b) => fold_float_bin(get(a), get(b), ty, |x, y| x / y),
        InstKind::FNeg(a) => fold_float_un(get(a), ty, |x| -x),
        InstKind::FAbs(a) => fold_float_un(get(a), ty, |x| x.abs()),
        InstKind::FSqrt(a) => fold_float_un(get(a), ty, |x| x.sqrt()),
        InstKind::FPow(a, b) => fold_float_bin(get(a), get(b), ty, |x, y| x.powf(y)),

        // Comparisons -----------------------------------------------------
        InstKind::ICmp(op, a, b) => {
            if let (Some(Const::Int(av, aw)), Some(Const::Int(bv, bw))) = (get(a), get(b)) {
                // Audit M-D: each operand must be sign-extended at
                // its OWN declared width. The earlier version reused
                // `a`'s width for both operands, which is correct
                // only when the verifier guarantees both operands
                // have the same width — and the verifier currently
                // only checks `is_int()`, not bit width. After a
                // future GVN-style pass introduces width drift this
                // becomes a wrong-result hazard.
                let av = sext(av, aw.bits());
                let bv = sext(bv, bw.bits());
                let r = match op {
                    CmpOp::Eq => av == bv,
                    CmpOp::Ne => av != bv,
                    CmpOp::Lt => av < bv,
                    CmpOp::Le => av <= bv,
                    CmpOp::Gt => av > bv,
                    CmpOp::Ge => av >= bv,
                };
                return Some(InstKind::ConstBool(r));
            }
            None
        }
        InstKind::FCmp(op, a, b) => {
            if let (Some(Const::Float(av, aw)), Some(Const::Float(bv, bw))) = (get(a), get(b)) {
                // Audit B-3 (defense in depth): explicitly round each
                // operand at its declared width before comparing.
                // Today every Const::Float producer respects the
                // M-1 invariant that f32-tagged values are already
                // f32-rounded — but doing the rounding here protects
                // future folds from a bug that breaks the invariant.
                let av = round_for_width(av, aw);
                let bv = round_for_width(bv, bw);
                let r = match op {
                    CmpOp::Eq => av == bv,
                    CmpOp::Ne => av != bv,
                    CmpOp::Lt => av < bv,
                    CmpOp::Le => av <= bv,
                    CmpOp::Gt => av > bv,
                    CmpOp::Ge => av >= bv,
                };
                return Some(InstKind::ConstBool(r));
            }
            None
        }

        // Logic on bools --------------------------------------------------
        InstKind::And(a, b) => {
            if let (Some(Const::Bool(av)), Some(Const::Bool(bv))) = (get(a), get(b)) {
                return Some(InstKind::ConstBool(av && bv));
            }
            None
        }
        InstKind::Or(a, b) => {
            if let (Some(Const::Bool(av)), Some(Const::Bool(bv))) = (get(a), get(b)) {
                return Some(InstKind::ConstBool(av || bv));
            }
            None
        }
        InstKind::Not(a) => {
            if let Some(Const::Bool(av)) = get(a) {
                return Some(InstKind::ConstBool(!av));
            }
            None
        }

        // Bitwise ---------------------------------------------------------
        InstKind::BitAnd(a, b) => fold_int_bin(get(a), get(b), ty, |x, y| x & y),
        InstKind::BitOr(a, b) => fold_int_bin(get(a), get(b), ty, |x, y| x | y),
        InstKind::BitXor(a, b) => fold_int_bin(get(a), get(b), ty, |x, y| x ^ y),
        InstKind::BitNot(a) => {
            if let Some(Const::Int(av, _)) = get(a) {
                if let IrType::Int(w) = ty {
                    return Some(InstKind::ConstInt(norm(!av, *w), *w));
                }
            }
            None
        }
        // Audit M-B / M-C: shift counts that are negative or ≥ the
        // operand width are *implementation-defined* in the IR. We
        // deliberately bail (return None) so the runtime / codegen
        // path is the single source of truth. AArch64 LSL/LSR/ASR
        // mask the count to the low log2(width) bits — emulating
        // that here is risky because Fortran ISHFT lowering computes
        // both branches and selects, and a bogus folded value could
        // leak through CSE / strength_reduce / other passes.
        InstKind::Shl(a, b) => {
            if let (Some(Const::Int(av, _)), Some(Const::Int(bv, _))) = (get(a), get(b)) {
                if let IrType::Int(w) = ty {
                    let bits = w.bits() as i128;
                    if (0..bits).contains(&bv) {
                        return Some(InstKind::ConstInt(norm(av.wrapping_shl(bv as u32), *w), *w));
                    }
                    return None;
                }
            }
            None
        }
        InstKind::LShr(a, b) => {
            if let (Some(Const::Int(av, _)), Some(Const::Int(bv, _))) = (get(a), get(b)) {
                if let IrType::Int(w) = ty {
                    let bits = w.bits() as i128;
                    if (0..bits).contains(&bv) {
                        let unsigned = (mask(av, w.bits()) as u128) >> (bv as u32);
                        return Some(InstKind::ConstInt(norm(unsigned as i128, *w), *w));
                    }
                    return None;
                }
            }
            None
        }
        InstKind::AShr(a, b) => {
            if let (Some(Const::Int(av, _)), Some(Const::Int(bv, _))) = (get(a), get(b)) {
                if let IrType::Int(w) = ty {
                    let bits = w.bits() as i128;
                    if (0..bits).contains(&bv) {
                        let signed = sext(av, w.bits()) >> (bv as u32);
                        return Some(InstKind::ConstInt(norm(signed, *w), *w));
                    }
                    return None;
                }
            }
            None
        }
        // Audit Med-3: the count uses the *source* operand's width
        // for the bit-mask but emits the result tagged with `inst.ty`'s
        // width (taken from the IR declaration), not the source's.
        // Today they always match because the lowerer keeps them in
        // sync, but a future change that makes popcount return i32
        // unconditionally (matching C intrinsics) would silently
        // produce a wrong-width constant if we kept reading from `w`.
        InstKind::PopCount(a) => {
            if let (Some(Const::Int(av, src_w)), IrType::Int(out_w)) = (get(a), ty) {
                let val = mask(av, src_w.bits()) as u128;
                return Some(InstKind::ConstInt(
                    norm(val.count_ones() as i128, *out_w),
                    *out_w,
                ));
            }
            None
        }
        InstKind::CountLeadingZeros(a) => {
            if let (Some(Const::Int(av, src_w)), IrType::Int(out_w)) = (get(a), ty) {
                let bits = src_w.bits();
                let val = mask(av, bits) as u128;
                let lz = if val == 0 {
                    bits as i128
                } else {
                    (val.leading_zeros() as i128) - (128 - bits as i128)
                };
                return Some(InstKind::ConstInt(norm(lz, *out_w), *out_w));
            }
            None
        }
        InstKind::CountTrailingZeros(a) => {
            if let (Some(Const::Int(av, src_w)), IrType::Int(out_w)) = (get(a), ty) {
                let bits = src_w.bits();
                let val = mask(av, bits) as u128;
                let tz = if val == 0 {
                    bits as i128
                } else {
                    val.trailing_zeros() as i128
                };
                return Some(InstKind::ConstInt(norm(tz, *out_w), *out_w));
            }
            None
        }

        // Conversions -----------------------------------------------------
        InstKind::IntToFloat(a, fw) => {
            if let Some(Const::Int(av, w)) = get(a) {
                let signed = sext(av, w.bits());
                // Round through the destination precision so that
                // downstream `const_fold` operations see the exact
                // value the runtime would observe. AArch64 `SCVTF`
                // produces an f32 directly for f32 destinations; we
                // approximate that by going through `f32` then back
                // to `f64` for IR storage. The double-rounding from
                // `i64 → f64 → f32` differs from a hypothetical
                // direct `i64 → f32` by at most one ULP near
                // round-tie boundaries — acceptable for now;
                // codegen always does the direct conversion at
                // runtime, so the only divergence is in IR-level
                // operations chained off of this constant.
                let v = match fw {
                    FloatWidth::F32 => signed as f32 as f64,
                    FloatWidth::F64 => signed as f64,
                };
                return Some(InstKind::ConstFloat(v, *fw));
            }
            None
        }
        InstKind::FloatToInt(a, w) => {
            if let Some(Const::Float(av, src_fw)) = get(a) {
                if av.is_nan() || av.is_infinite() {
                    return None;
                }
                // Audit B-3 (defense in depth): re-round at the
                // source float width before truncating. The M-1
                // invariant says any f32-tagged Const::Float is
                // already f32-rounded, but doing it explicitly here
                // means a future invariant break can't silently
                // produce a wrong int.
                let av = round_for_width(av, src_fw);
                // Fortran INT() truncates toward zero. Match.
                let truncd = av.trunc();
                let lo = match w {
                    IntWidth::I8 => i8::MIN as f64,
                    IntWidth::I16 => i16::MIN as f64,
                    IntWidth::I32 => i32::MIN as f64,
                    IntWidth::I64 => i64::MIN as f64,
                    IntWidth::I128 => return None,
                };
                let hi = match w {
                    IntWidth::I8 => i8::MAX as f64,
                    IntWidth::I16 => i16::MAX as f64,
                    IntWidth::I32 => i32::MAX as f64,
                    IntWidth::I64 => i64::MAX as f64,
                    IntWidth::I128 => return None,
                };
                if truncd < lo || truncd > hi {
                    return None;
                }
                return Some(InstKind::ConstInt(norm(truncd as i128, *w), *w));
            }
            None
        }
        InstKind::FloatExtend(a, fw) | InstKind::FloatTrunc(a, fw) => {
            if let Some(Const::Float(av, _)) = get(a) {
                let v = match fw {
                    FloatWidth::F32 => av as f32 as f64, // round through f32
                    FloatWidth::F64 => av,
                };
                return Some(InstKind::ConstFloat(v, *fw));
            }
            None
        }
        InstKind::IntExtend(a, w, signed) => {
            if let Some(Const::Int(av, src_w)) = get(a) {
                let v = if *signed {
                    sext(av, src_w.bits())
                } else {
                    mask(av, src_w.bits())
                };
                return Some(InstKind::ConstInt(norm(v, *w), *w));
            }
            None
        }
        InstKind::IntTrunc(a, w) => {
            if let Some(Const::Int(av, _)) = get(a) {
                return Some(InstKind::ConstInt(norm(av, *w), *w));
            }
            None
        }

        // Select on a known condition ------------------------------------
        InstKind::Select(c, t, f) => {
            if let Some(Const::Bool(cv)) = get(c) {
                let chosen = if cv { *t } else { *f };
                if let Some(k) = consts.get(&chosen) {
                    // Re-emit using the **destination** type carried
                    // by the Select instruction itself, NOT the
                    // chosen branch's source type. After IntExtend /
                    // FloatTrunc / similar fold chains the chosen
                    // operand can carry a narrower width than the
                    // Select declares; reusing it would silently
                    // produce a `kind` whose embedded width disagrees
                    // with `inst.ty`. Audit C-C.
                    return match (ty, *k) {
                        (IrType::Int(w), Const::Int(v, src_w)) => {
                            // Audit B-1: sign-extend the chosen
                            // value at its **source** width before
                            // re-norming at the destination. The
                            // earlier version discarded `src_w` and
                            // treated the raw stored bits as already
                            // sign-extended at the destination width,
                            // silently producing a wrong-signed value
                            // when widths differed. Example:
                            // chosen = ConstInt(255, I8) (i.e. -1
                            // at i8 precision); destination = I64.
                            // Without source-width sext, we'd emit
                            // ConstInt(255, I64) instead of
                            // ConstInt(-1, I64).
                            let signed = sext(v, src_w.bits());
                            Some(InstKind::ConstInt(norm(signed, *w), *w))
                        }
                        (IrType::Float(FloatWidth::F32), Const::Float(v, _)) => {
                            Some(InstKind::ConstFloat(v as f32 as f64, FloatWidth::F32))
                        }
                        (IrType::Float(FloatWidth::F64), Const::Float(v, _)) => {
                            Some(InstKind::ConstFloat(v, FloatWidth::F64))
                        }
                        (IrType::Bool, Const::Bool(b)) => Some(InstKind::ConstBool(b)),
                        // Type categories disagree — bail rather than
                        // type-pun. The verifier doesn't catch the
                        // mismatch (it only checks `is_int`/`is_float`
                        // not specific widths), so being conservative
                        // here is the correctness floor.
                        _ => None,
                    };
                }
            }
            None
        }

        _ => None,
    }
}

// fold_float_bin / fold_float_un rely on the N-9 invariant:
// `Const::from_inst` normalizes every incoming `Const::Float` via
// `round_for_width`, so `av` / `bv` below are already exact at their
// declared precision. This lets us operate on the raw f64 storage
// without re-rounding per input — the N-3 defense-in-depth concern
// raised in the third audit is satisfied at the producer side rather
// than repeated at every use site. We DO still round the result for
// f32 destinations because the op itself can produce a value
// outside f32's representable range.
fn fold_float_bin<F>(a: Option<Const>, b: Option<Const>, ty: &IrType, op: F) -> Option<InstKind>
where
    F: FnOnce(f64, f64) -> f64,
{
    if let (Some(Const::Float(av, _)), Some(Const::Float(bv, _))) = (a, b) {
        if let IrType::Float(w) = ty {
            let r = op(av, bv);
            let r = round_for_width(r, *w);
            return Some(InstKind::ConstFloat(r, *w));
        }
    }
    None
}

fn fold_float_un<F>(a: Option<Const>, ty: &IrType, op: F) -> Option<InstKind>
where
    F: FnOnce(f64) -> f64,
{
    if let Some(Const::Float(av, _)) = a {
        if let IrType::Float(w) = ty {
            let r = op(av);
            let r = round_for_width(r, *w);
            return Some(InstKind::ConstFloat(r, *w));
        }
    }
    None
}

fn fold_int_bin<F>(a: Option<Const>, b: Option<Const>, ty: &IrType, op: F) -> Option<InstKind>
where
    F: FnOnce(i128, i128) -> i128,
{
    if let (Some(Const::Int(av, _)), Some(Const::Int(bv, _))) = (a, b) {
        if let IrType::Int(w) = ty {
            return Some(InstKind::ConstInt(norm(op(av, bv), *w), *w));
        }
    }
    None
}

/// The constant folding pass.
pub struct ConstFold;

impl Pass for ConstFold {
    fn name(&self) -> &'static str {
        "const-fold"
    }

    fn run(&self, module: &mut Module) -> bool {
        let rounding_effects = super::fpenv::analyze_rounding_effects(module);
        let mut changed = false;
        for (func_idx, func) in module.functions.iter_mut().enumerate() {
            let fpenv_barrier = rounding_effects.may_run_after_change[func_idx];

            // Audit N-8: we walk `func.blocks` in vec order, which
            // is NOT guaranteed to be reverse-postorder. If a fold
            // in an early-vec block needs a constant defined by a
            // later-vec block (that still dominates it in the CFG),
            // the earlier version would miss the fold — and the
            // outer pass-manager fixpoint would NOT rescue us when
            // no other pass in the pipeline produced a change.
            //
            // Fix: two phases.
            //
            //  1. Pre-collect every `Const*` instruction's value so
            //     the map is fully populated before any fold runs.
            //  2. Iterate the fold walk to a local fixpoint. A fold
            //     that creates a new constant is added to the map
            //     on the fly, unlocking downstream folds in the
            //     next iteration. Each iteration is monotonic (the
            //     consts map only grows), so the inner loop is
            //     bounded by O(number of foldable instructions).
            let mut consts: HashMap<ValueId, Const> = HashMap::new();
            for block in &func.blocks {
                for inst in &block.insts {
                    if let Some(c) = Const::from_inst(&inst.kind) {
                        consts.insert(inst.id, c);
                    }
                }
            }

            let mut inner_changed = true;
            while inner_changed {
                inner_changed = false;
                for block in &mut func.blocks {
                    for inst in &mut block.insts {
                        if consts.contains_key(&inst.id) {
                            continue;
                        }
                        if let Some(new_kind) =
                            try_fold(&inst.kind, &inst.ty, &consts, fpenv_barrier)
                        {
                            if let Some(c) = Const::from_inst(&new_kind) {
                                consts.insert(inst.id, c);
                            }
                            inst.kind = new_kind;
                            changed = true;
                            inner_changed = true;
                        }
                    }
                }
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{FuncSig, IrType};

    fn dummy_span() -> crate::lexer::Span {
        let p = crate::lexer::Position { line: 1, col: 1 };
        crate::lexer::Span {
            start: p,
            end: p,
            file_id: 0,
        }
    }

    fn make_module_with(insts: Vec<(InstKind, IrType)>) -> (Module, Vec<ValueId>) {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let entry = f.entry;
        let mut ids = Vec::new();
        for (kind, ty) in insts {
            let id = f.next_value_id();
            ids.push(id);
            f.block_mut(entry).insts.push(Inst {
                id,
                kind,
                ty,
                span: dummy_span(),
            });
        }
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        (m, ids)
    }

    fn first_block_kinds(m: &Module) -> Vec<InstKind> {
        m.functions[0].blocks[0]
            .insts
            .iter()
            .map(|i| i.kind.clone())
            .collect()
    }

    #[test]
    fn folds_iadd_i32() {
        let (mut m, ids) = make_module_with(vec![
            (
                InstKind::ConstInt(3, IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::ConstInt(4, IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::IAdd(ValueId(0), ValueId(1)),
                IrType::Int(IntWidth::I32),
            ),
        ]);
        let _ = ids;
        assert!(ConstFold.run(&mut m));
        let kinds = first_block_kinds(&m);
        assert!(matches!(kinds[2], InstKind::ConstInt(7, IntWidth::I32)));
    }

    #[test]
    fn integer_overflow_wraps_at_width() {
        // i8: 100 + 50 = 150 → wraps to -106
        let (mut m, _) = make_module_with(vec![
            (
                InstKind::ConstInt(100, IntWidth::I8),
                IrType::Int(IntWidth::I8),
            ),
            (
                InstKind::ConstInt(50, IntWidth::I8),
                IrType::Int(IntWidth::I8),
            ),
            (
                InstKind::IAdd(ValueId(0), ValueId(1)),
                IrType::Int(IntWidth::I8),
            ),
        ]);
        assert!(ConstFold.run(&mut m));
        let kinds = first_block_kinds(&m);
        match kinds[2] {
            InstKind::ConstInt(v, IntWidth::I8) => assert_eq!(v, -106),
            _ => panic!("expected ConstInt"),
        }
    }

    #[test]
    fn idiv_by_zero_left_alone() {
        let (mut m, _) = make_module_with(vec![
            (
                InstKind::ConstInt(10, IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::ConstInt(0, IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::IDiv(ValueId(0), ValueId(1)),
                IrType::Int(IntWidth::I32),
            ),
        ]);
        assert!(!ConstFold.run(&mut m));
        let kinds = first_block_kinds(&m);
        assert!(matches!(kinds[2], InstKind::IDiv(..)));
    }

    #[test]
    fn fmul_f64() {
        let (mut m, _) = make_module_with(vec![
            (
                InstKind::ConstFloat(2.0, FloatWidth::F64),
                IrType::Float(FloatWidth::F64),
            ),
            (
                InstKind::ConstFloat(std::f64::consts::PI, FloatWidth::F64),
                IrType::Float(FloatWidth::F64),
            ),
            (
                InstKind::FMul(ValueId(0), ValueId(1)),
                IrType::Float(FloatWidth::F64),
            ),
        ]);
        assert!(ConstFold.run(&mut m));
        let kinds = first_block_kinds(&m);
        match kinds[2] {
            InstKind::ConstFloat(v, FloatWidth::F64) => {
                assert!((v - std::f64::consts::TAU).abs() < 1e-12)
            }
            _ => panic!("expected ConstFloat"),
        }
    }

    #[test]
    fn leaves_rounding_dependent_fold_after_fpenv_change() {
        let (mut m, _) = make_module_with(vec![
            (
                InstKind::ConstFloat(1.0, FloatWidth::F64),
                IrType::Float(FloatWidth::F64),
            ),
            (
                InstKind::ConstFloat(5.551_115_123_125_783e-17, FloatWidth::F64),
                IrType::Float(FloatWidth::F64),
            ),
            (
                InstKind::ConstInt(2, IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::Call(
                    FuncRef::External("afs_ieee_set_rounding".into()),
                    vec![ValueId(2)],
                ),
                IrType::Void,
            ),
            (
                InstKind::FAdd(ValueId(0), ValueId(1)),
                IrType::Float(FloatWidth::F64),
            ),
        ]);

        assert!(
            !ConstFold.run(&mut m),
            "constant FP arithmetic must remain dynamic when the function changes rounding mode"
        );
        assert!(matches!(first_block_kinds(&m)[4], InstKind::FAdd(..)));
    }

    #[test]
    fn leaves_rounding_dependent_fold_in_called_function() {
        let (mut m, _) = make_module_with(vec![
            (
                InstKind::ConstFloat(1.0, FloatWidth::F64),
                IrType::Float(FloatWidth::F64),
            ),
            (
                InstKind::ConstFloat(5.551_115_123_125_783e-17, FloatWidth::F64),
                IrType::Float(FloatWidth::F64),
            ),
            (
                InstKind::FAdd(ValueId(0), ValueId(1)),
                IrType::Float(FloatWidth::F64),
            ),
        ]);
        let mut caller = Function::new("caller".into(), vec![], IrType::Void);
        let entry = caller.entry;
        for callee in [
            FuncRef::External("afs_ieee_set_rounding".into()),
            FuncRef::Internal(0),
        ] {
            let id = caller.next_value_id();
            caller.block_mut(entry).insts.push(Inst {
                id,
                kind: InstKind::Call(callee, vec![]),
                ty: IrType::Void,
                span: dummy_span(),
            });
        }
        caller.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(caller);

        assert!(
            !ConstFold.run(&mut m),
            "a callee can execute under the rounding mode selected by its caller"
        );
        assert!(matches!(
            m.functions[0].blocks[0].insts[2].kind,
            InstKind::FAdd(..)
        ));
    }

    #[test]
    fn leaves_rounding_dependent_fold_in_indirect_target() {
        let (mut m, _) = make_module_with(vec![
            (
                InstKind::ConstFloat(1.0, FloatWidth::F64),
                IrType::Float(FloatWidth::F64),
            ),
            (
                InstKind::ConstFloat(5.551_115_123_125_783e-17, FloatWidth::F64),
                IrType::Float(FloatWidth::F64),
            ),
            (
                InstKind::FAdd(ValueId(0), ValueId(1)),
                IrType::Float(FloatWidth::F64),
            ),
        ]);
        let mut caller = Function::new("caller".into(), vec![], IrType::Void);
        let entry = caller.entry;
        let setter = caller.next_value_id();
        caller.block_mut(entry).insts.push(Inst {
            id: setter,
            kind: InstKind::Call(FuncRef::External("afs_ieee_set_rounding".into()), vec![]),
            ty: IrType::Void,
            span: dummy_span(),
        });
        let target_ty = IrType::FuncPtr(Box::new(FuncSig {
            params: vec![],
            ret: IrType::Void,
        }));
        let target = caller.next_value_id();
        caller.block_mut(entry).insts.push(Inst {
            id: target,
            kind: InstKind::Undef(target_ty.clone()),
            ty: target_ty,
            span: dummy_span(),
        });
        let call = caller.next_value_id();
        caller.block_mut(entry).insts.push(Inst {
            id: call,
            kind: InstKind::Call(FuncRef::Indirect(target), vec![]),
            ty: IrType::Void,
            span: dummy_span(),
        });
        caller.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(caller);

        assert!(
            !ConstFold.run(&mut m),
            "an indirect call can enter a function under a changed rounding mode"
        );
        assert!(matches!(
            m.functions[0].blocks[0].insts[2].kind,
            InstKind::FAdd(..)
        ));
    }

    #[test]
    fn icmp_eq_true() {
        let (mut m, _) = make_module_with(vec![
            (
                InstKind::ConstInt(5, IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::ConstInt(5, IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::ICmp(CmpOp::Eq, ValueId(0), ValueId(1)),
                IrType::Bool,
            ),
        ]);
        assert!(ConstFold.run(&mut m));
        assert!(matches!(
            first_block_kinds(&m)[2],
            InstKind::ConstBool(true)
        ));
    }

    #[test]
    fn icmp_signed_lt_with_i8_negatives() {
        // i8 representation: -1 stored as 0xff (255). Without sign-extension,
        // a naive (av < bv) would compare 255 < 1 → false; correct is true.
        let (mut m, _) = make_module_with(vec![
            (
                InstKind::ConstInt(-1, IntWidth::I8),
                IrType::Int(IntWidth::I8),
            ),
            (
                InstKind::ConstInt(1, IntWidth::I8),
                IrType::Int(IntWidth::I8),
            ),
            (
                InstKind::ICmp(CmpOp::Lt, ValueId(0), ValueId(1)),
                IrType::Bool,
            ),
        ]);
        assert!(ConstFold.run(&mut m));
        assert!(matches!(
            first_block_kinds(&m)[2],
            InstKind::ConstBool(true)
        ));
    }

    #[test]
    fn shl_power_of_two() {
        let (mut m, _) = make_module_with(vec![
            (
                InstKind::ConstInt(1, IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::ConstInt(3, IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::Shl(ValueId(0), ValueId(1)),
                IrType::Int(IntWidth::I32),
            ),
        ]);
        assert!(ConstFold.run(&mut m));
        match first_block_kinds(&m)[2] {
            InstKind::ConstInt(v, IntWidth::I32) => assert_eq!(v, 8),
            _ => panic!(),
        }
    }

    #[test]
    fn int_to_float_chain() {
        // const(42:i32) → int_to_float → const(42.0:f64)
        let (mut m, _) = make_module_with(vec![
            (
                InstKind::ConstInt(42, IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::IntToFloat(ValueId(0), FloatWidth::F64),
                IrType::Float(FloatWidth::F64),
            ),
        ]);
        assert!(ConstFold.run(&mut m));
        match first_block_kinds(&m)[1] {
            InstKind::ConstFloat(v, FloatWidth::F64) => assert_eq!(v, 42.0),
            _ => panic!(),
        }
    }

    #[test]
    fn select_on_known_cond_picks_branch() {
        let (mut m, _) = make_module_with(vec![
            (InstKind::ConstBool(true), IrType::Bool),
            (
                InstKind::ConstInt(10, IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::ConstInt(20, IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::Select(ValueId(0), ValueId(1), ValueId(2)),
                IrType::Int(IntWidth::I32),
            ),
        ]);
        assert!(ConstFold.run(&mut m));
        match first_block_kinds(&m)[3] {
            InstKind::ConstInt(v, IntWidth::I32) => assert_eq!(v, 10),
            _ => panic!(),
        }
    }

    #[test]
    fn fixpoint_via_chained_constants() {
        // (3 + 4) * 2 = 14, where the inner add must fold first.
        let (mut m, _) = make_module_with(vec![
            (
                InstKind::ConstInt(3, IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::ConstInt(4, IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::IAdd(ValueId(0), ValueId(1)),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::ConstInt(2, IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
            (
                InstKind::IMul(ValueId(2), ValueId(3)),
                IrType::Int(IntWidth::I32),
            ),
        ]);
        // A single pass already handles this because we walk in order.
        assert!(ConstFold.run(&mut m));
        let kinds = first_block_kinds(&m);
        assert!(matches!(kinds[2], InstKind::ConstInt(7, IntWidth::I32)));
        assert!(matches!(kinds[4], InstKind::ConstInt(14, IntWidth::I32)));
    }

    #[test]
    fn nan_float_to_int_left_alone() {
        let (mut m, _) = make_module_with(vec![
            (
                InstKind::ConstFloat(f64::NAN, FloatWidth::F64),
                IrType::Float(FloatWidth::F64),
            ),
            (
                InstKind::FloatToInt(ValueId(0), IntWidth::I32),
                IrType::Int(IntWidth::I32),
            ),
        ]);
        assert!(!ConstFold.run(&mut m));
        assert!(matches!(first_block_kinds(&m)[1], InstKind::FloatToInt(..)));
    }
}
