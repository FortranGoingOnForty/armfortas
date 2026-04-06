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
use crate::ir::types::{IrType, IntWidth, FloatWidth};
use std::collections::HashMap;

/// Compile-time constant value, normalized for folding.
#[derive(Debug, Clone, Copy)]
enum Const {
    Int(i64, IntWidth),
    Float(f64, FloatWidth),
    Bool(bool),
}

impl Const {
    fn from_inst(kind: &InstKind) -> Option<Self> {
        match kind {
            InstKind::ConstInt(v, w) => Some(Const::Int(*v, *w)),
            InstKind::ConstFloat(v, w) => Some(Const::Float(*v, *w)),
            InstKind::ConstBool(b) => Some(Const::Bool(*b)),
            _ => None,
        }
    }
}

/// Sign-extend `v` from `bits` to a full i64. Used so that comparisons
/// and arithmetic on narrower integer types match hardware behavior.
fn sext(v: i64, bits: u32) -> i64 {
    if bits >= 64 { v }
    else {
        let shift = 64 - bits;
        (v << shift) >> shift
    }
}

/// Mask `v` down to `bits` low bits. The IR stores integers as i64 but
/// arithmetic must wrap at the declared width.
fn mask(v: i64, bits: u32) -> i64 {
    if bits >= 64 { v }
    else { v & ((1i64 << bits) - 1) }
}

/// Truncate-then-sign-extend a wide arithmetic result back to its
/// declared width, normalized to a sign-extended i64.
fn norm(v: i64, w: IntWidth) -> i64 {
    sext(mask(v, w.bits()), w.bits())
}

/// Try to evaluate a single instruction given a map of known
/// constants. Returns `Some(new_kind)` if the instruction can be
/// replaced with a `Const*`.
#[allow(clippy::too_many_lines)]
fn try_fold(kind: &InstKind, ty: &IrType, consts: &HashMap<ValueId, Const>) -> Option<InstKind> {
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
                if bv == 0 { return None; } // leave divide-by-zero to runtime
                // i64::MIN / -1 also overflows; bail to keep behavior identical.
                if av == i64::MIN && bv == -1 { return None; }
                if let IrType::Int(w) = ty {
                    return Some(InstKind::ConstInt(norm(av / bv, *w), *w));
                }
            }
            None
        }
        InstKind::IMod(a, b) => {
            if let (Some(Const::Int(av, _)), Some(Const::Int(bv, _))) = (get(a), get(b)) {
                if bv == 0 { return None; }
                if av == i64::MIN && bv == -1 { return None; }
                if let IrType::Int(w) = ty {
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
            if let (Some(Const::Int(av, w)), Some(Const::Int(bv, _))) = (get(a), get(b)) {
                let av = sext(av, w.bits());
                let bv = sext(bv, w.bits());
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
            if let (Some(Const::Float(av, _)), Some(Const::Float(bv, _))) = (get(a), get(b)) {
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
        InstKind::BitOr(a, b)  => fold_int_bin(get(a), get(b), ty, |x, y| x | y),
        InstKind::BitXor(a, b) => fold_int_bin(get(a), get(b), ty, |x, y| x ^ y),
        InstKind::BitNot(a) => {
            if let Some(Const::Int(av, _)) = get(a) {
                if let IrType::Int(w) = ty {
                    return Some(InstKind::ConstInt(norm(!av, *w), *w));
                }
            }
            None
        }
        InstKind::Shl(a, b) => {
            if let (Some(Const::Int(av, _)), Some(Const::Int(bv, _))) = (get(a), get(b)) {
                if let IrType::Int(w) = ty {
                    let bits = w.bits() as i64;
                    if (0..bits).contains(&bv) {
                        return Some(InstKind::ConstInt(
                            norm(av.wrapping_shl(bv as u32), *w),
                            *w,
                        ));
                    }
                    // Shift count >= width is implementation-defined
                    // in C; AArch64 returns 0. Match that.
                    return Some(InstKind::ConstInt(0, *w));
                }
            }
            None
        }
        InstKind::LShr(a, b) => {
            if let (Some(Const::Int(av, _)), Some(Const::Int(bv, _))) = (get(a), get(b)) {
                if let IrType::Int(w) = ty {
                    let bits = w.bits();
                    let bv64 = bv as u32;
                    if bv64 < bits {
                        let unsigned = (mask(av, bits) as u64) >> bv64;
                        return Some(InstKind::ConstInt(norm(unsigned as i64, *w), *w));
                    }
                    return Some(InstKind::ConstInt(0, *w));
                }
            }
            None
        }
        InstKind::AShr(a, b) => {
            if let (Some(Const::Int(av, _)), Some(Const::Int(bv, _))) = (get(a), get(b)) {
                if let IrType::Int(w) = ty {
                    let bits = w.bits();
                    let bv32 = bv as u32;
                    if bv32 < bits {
                        let signed = sext(av, bits) >> bv32;
                        return Some(InstKind::ConstInt(norm(signed, *w), *w));
                    }
                    // Arithmetic shift past the width: replicate sign bit.
                    let signed = sext(av, bits) >> (bits - 1);
                    return Some(InstKind::ConstInt(norm(signed, *w), *w));
                }
            }
            None
        }
        InstKind::PopCount(a) => {
            if let Some(Const::Int(av, w)) = get(a) {
                let val = mask(av, w.bits()) as u64;
                return Some(InstKind::ConstInt(val.count_ones() as i64, w));
            }
            None
        }
        InstKind::CountLeadingZeros(a) => {
            if let Some(Const::Int(av, w)) = get(a) {
                let bits = w.bits();
                let val = mask(av, bits) as u64;
                let lz = if val == 0 {
                    bits as i64
                } else {
                    // Leading-zeros within `bits` width.
                    (val.leading_zeros() as i64) - (64 - bits as i64)
                };
                return Some(InstKind::ConstInt(lz, w));
            }
            None
        }
        InstKind::CountTrailingZeros(a) => {
            if let Some(Const::Int(av, w)) = get(a) {
                let bits = w.bits();
                let val = mask(av, bits) as u64;
                let tz = if val == 0 { bits as i64 } else { val.trailing_zeros() as i64 };
                return Some(InstKind::ConstInt(tz, w));
            }
            None
        }

        // Conversions -----------------------------------------------------
        InstKind::IntToFloat(a, fw) => {
            if let Some(Const::Int(av, w)) = get(a) {
                let signed = sext(av, w.bits());
                return Some(InstKind::ConstFloat(signed as f64, *fw));
            }
            None
        }
        InstKind::FloatToInt(a, w) => {
            if let Some(Const::Float(av, _)) = get(a) {
                if av.is_nan() || av.is_infinite() { return None; }
                // Fortran INT() truncates toward zero. Match.
                let truncd = av.trunc();
                let lo = match w {
                    IntWidth::I8  => i8::MIN  as f64,
                    IntWidth::I16 => i16::MIN as f64,
                    IntWidth::I32 => i32::MIN as f64,
                    IntWidth::I64 => i64::MIN as f64,
                };
                let hi = match w {
                    IntWidth::I8  => i8::MAX  as f64,
                    IntWidth::I16 => i16::MAX as f64,
                    IntWidth::I32 => i32::MAX as f64,
                    IntWidth::I64 => i64::MAX as f64,
                };
                if truncd < lo || truncd > hi { return None; }
                return Some(InstKind::ConstInt(norm(truncd as i64, *w), *w));
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
                let v = if *signed { sext(av, src_w.bits()) } else { mask(av, src_w.bits()) };
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
                    // Re-emit as the corresponding constant kind.
                    return Some(match *k {
                        Const::Int(v, w) => InstKind::ConstInt(v, w),
                        Const::Float(v, w) => InstKind::ConstFloat(v, w),
                        Const::Bool(b) => InstKind::ConstBool(b),
                    });
                }
            }
            None
        }

        _ => None,
    }
}

fn fold_float_bin<F>(a: Option<Const>, b: Option<Const>, ty: &IrType, op: F) -> Option<InstKind>
where F: FnOnce(f64, f64) -> f64
{
    if let (Some(Const::Float(av, _)), Some(Const::Float(bv, _))) = (a, b) {
        if let IrType::Float(w) = ty {
            let r = op(av, bv);
            // For f32 destinations, round through f32 to match codegen.
            let r = match w {
                FloatWidth::F32 => r as f32 as f64,
                FloatWidth::F64 => r,
            };
            return Some(InstKind::ConstFloat(r, *w));
        }
    }
    None
}

fn fold_float_un<F>(a: Option<Const>, ty: &IrType, op: F) -> Option<InstKind>
where F: FnOnce(f64) -> f64
{
    if let Some(Const::Float(av, _)) = a {
        if let IrType::Float(w) = ty {
            let r = op(av);
            let r = match w { FloatWidth::F32 => r as f32 as f64, FloatWidth::F64 => r };
            return Some(InstKind::ConstFloat(r, *w));
        }
    }
    None
}

fn fold_int_bin<F>(a: Option<Const>, b: Option<Const>, ty: &IrType, op: F) -> Option<InstKind>
where F: FnOnce(i64, i64) -> i64
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
    fn name(&self) -> &'static str { "const-fold" }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            // Map of value-id → constant. Built and updated in lockstep
            // with the walk so that a fold can feed later folds in the
            // same block (SSA gives us the no-forward-refs guarantee).
            let mut consts: HashMap<ValueId, Const> = HashMap::new();
            for block in &mut func.blocks {
                for inst in &mut block.insts {
                    if let Some(c) = Const::from_inst(&inst.kind) {
                        consts.insert(inst.id, c);
                        continue;
                    }
                    if let Some(new_kind) = try_fold(&inst.kind, &inst.ty, &consts) {
                        if let Some(c) = Const::from_inst(&new_kind) {
                            consts.insert(inst.id, c);
                        }
                        inst.kind = new_kind;
                        changed = true;
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
    use crate::ir::types::IrType;

    fn dummy_span() -> crate::lexer::Span {
        let p = crate::lexer::Position { line: 1, col: 1 };
        crate::lexer::Span { start: p, end: p, file_id: 0 }
    }

    fn make_module_with(insts: Vec<(InstKind, IrType)>) -> (Module, Vec<ValueId>) {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let entry = f.entry;
        let mut ids = Vec::new();
        for (kind, ty) in insts {
            let id = f.next_value_id();
            ids.push(id);
            f.block_mut(entry).insts.push(Inst { id, kind, ty, span: dummy_span() });
        }
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        (m, ids)
    }

    fn first_block_kinds(m: &Module) -> Vec<InstKind> {
        m.functions[0].blocks[0].insts.iter().map(|i| i.kind.clone()).collect()
    }

    #[test]
    fn folds_iadd_i32() {
        let (mut m, ids) = make_module_with(vec![
            (InstKind::ConstInt(3, IntWidth::I32), IrType::Int(IntWidth::I32)),
            (InstKind::ConstInt(4, IntWidth::I32), IrType::Int(IntWidth::I32)),
            (InstKind::IAdd(ValueId(0), ValueId(1)), IrType::Int(IntWidth::I32)),
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
            (InstKind::ConstInt(100, IntWidth::I8), IrType::Int(IntWidth::I8)),
            (InstKind::ConstInt(50,  IntWidth::I8), IrType::Int(IntWidth::I8)),
            (InstKind::IAdd(ValueId(0), ValueId(1)), IrType::Int(IntWidth::I8)),
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
            (InstKind::ConstInt(10, IntWidth::I32), IrType::Int(IntWidth::I32)),
            (InstKind::ConstInt(0,  IntWidth::I32), IrType::Int(IntWidth::I32)),
            (InstKind::IDiv(ValueId(0), ValueId(1)), IrType::Int(IntWidth::I32)),
        ]);
        assert!(!ConstFold.run(&mut m));
        let kinds = first_block_kinds(&m);
        assert!(matches!(kinds[2], InstKind::IDiv(..)));
    }

    #[test]
    fn fmul_f64() {
        let (mut m, _) = make_module_with(vec![
            (InstKind::ConstFloat(2.0, FloatWidth::F64), IrType::Float(FloatWidth::F64)),
            (InstKind::ConstFloat(3.14, FloatWidth::F64), IrType::Float(FloatWidth::F64)),
            (InstKind::FMul(ValueId(0), ValueId(1)), IrType::Float(FloatWidth::F64)),
        ]);
        assert!(ConstFold.run(&mut m));
        let kinds = first_block_kinds(&m);
        match kinds[2] {
            InstKind::ConstFloat(v, FloatWidth::F64) => assert!((v - 6.28).abs() < 1e-12),
            _ => panic!("expected ConstFloat"),
        }
    }

    #[test]
    fn icmp_eq_true() {
        let (mut m, _) = make_module_with(vec![
            (InstKind::ConstInt(5, IntWidth::I32), IrType::Int(IntWidth::I32)),
            (InstKind::ConstInt(5, IntWidth::I32), IrType::Int(IntWidth::I32)),
            (InstKind::ICmp(CmpOp::Eq, ValueId(0), ValueId(1)), IrType::Bool),
        ]);
        assert!(ConstFold.run(&mut m));
        assert!(matches!(first_block_kinds(&m)[2], InstKind::ConstBool(true)));
    }

    #[test]
    fn icmp_signed_lt_with_i8_negatives() {
        // i8 representation: -1 stored as 0xff (255). Without sign-extension,
        // a naive (av < bv) would compare 255 < 1 → false; correct is true.
        let (mut m, _) = make_module_with(vec![
            (InstKind::ConstInt(-1, IntWidth::I8), IrType::Int(IntWidth::I8)),
            (InstKind::ConstInt(1,  IntWidth::I8), IrType::Int(IntWidth::I8)),
            (InstKind::ICmp(CmpOp::Lt, ValueId(0), ValueId(1)), IrType::Bool),
        ]);
        assert!(ConstFold.run(&mut m));
        assert!(matches!(first_block_kinds(&m)[2], InstKind::ConstBool(true)));
    }

    #[test]
    fn shl_power_of_two() {
        let (mut m, _) = make_module_with(vec![
            (InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32)),
            (InstKind::ConstInt(3, IntWidth::I32), IrType::Int(IntWidth::I32)),
            (InstKind::Shl(ValueId(0), ValueId(1)), IrType::Int(IntWidth::I32)),
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
            (InstKind::ConstInt(42, IntWidth::I32), IrType::Int(IntWidth::I32)),
            (InstKind::IntToFloat(ValueId(0), FloatWidth::F64), IrType::Float(FloatWidth::F64)),
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
            (InstKind::ConstInt(10, IntWidth::I32), IrType::Int(IntWidth::I32)),
            (InstKind::ConstInt(20, IntWidth::I32), IrType::Int(IntWidth::I32)),
            (InstKind::Select(ValueId(0), ValueId(1), ValueId(2)), IrType::Int(IntWidth::I32)),
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
            (InstKind::ConstInt(3, IntWidth::I32), IrType::Int(IntWidth::I32)),
            (InstKind::ConstInt(4, IntWidth::I32), IrType::Int(IntWidth::I32)),
            (InstKind::IAdd(ValueId(0), ValueId(1)), IrType::Int(IntWidth::I32)),
            (InstKind::ConstInt(2, IntWidth::I32), IrType::Int(IntWidth::I32)),
            (InstKind::IMul(ValueId(2), ValueId(3)), IrType::Int(IntWidth::I32)),
        ]);
        // A single pass already handles this because we walk in order.
        assert!(ConstFold.run(&mut m));
        let kinds = first_block_kinds(&m);
        assert!(matches!(kinds[2], InstKind::ConstInt(7,  IntWidth::I32)));
        assert!(matches!(kinds[4], InstKind::ConstInt(14, IntWidth::I32)));
    }

    #[test]
    fn nan_float_to_int_left_alone() {
        let (mut m, _) = make_module_with(vec![
            (InstKind::ConstFloat(f64::NAN, FloatWidth::F64), IrType::Float(FloatWidth::F64)),
            (InstKind::FloatToInt(ValueId(0), IntWidth::I32), IrType::Int(IntWidth::I32)),
        ]);
        assert!(!ConstFold.run(&mut m));
        assert!(matches!(first_block_kinds(&m)[1], InstKind::FloatToInt(..)));
    }
}
