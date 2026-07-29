//! Common-subexpression elimination (local).
//!
//! Within a basic block, two pure instructions that compute the same
//! expression on the same operands produce the same value. We can drop
//! the second one and rewrite its uses to point at the first. The
//! invariant we rely on is **SSA**: an instruction's `ValueId` never
//! changes once defined, so a downstream rewrite is just a textual
//! substitution.
//!
//! ## Why "local"
//!
//! Local CSE only matches duplicates inside a single basic block. It's
//! cheap, never crosses control-flow merges, and is provably correct
//! without alias analysis or dominance walks. A separate global-CSE
//! pass (or GVN, which subsumes CSE) will land later for matches
//! across dominating blocks.
//!
//! ## Side effects
//!
//! Only **pure** instructions are CSE candidates. We deliberately do
//! not deduplicate `Load` here — even though two loads of the same
//! address are usually equivalent, an intervening `Store` or `Call`
//! may have written the location. The future load-store-forwarding
//! pass will handle that with proper dependence analysis.
//!
//! ## Commutativity
//!
//! For commutative operators we canonicalize the operand pair so that
//! `iadd(a, b)` and `iadd(b, a)` collide on the same key. The
//! canonical form puts the smaller `ValueId` first. Comparisons
//! `Eq`/`Ne` are also commutative; `Lt`/`Le`/`Gt`/`Ge` are not.

use super::pass::Pass;
use super::util::{for_each_operand_mut, for_each_terminator_operand_mut};
use crate::ir::inst::*;
use crate::ir::types::IrType;
use std::collections::HashMap;

/// A canonical key for one pure instruction.
///
/// Two instructions producing the same key are guaranteed to compute
/// the same value (modulo type, which is also encoded). The integer
/// "tag" disambiguates instruction kinds; the rest of the tuple
/// encodes the operands.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
struct Key {
    tag: u32,
    operands: Vec<ValueId>,
    /// Auxiliary integer (used for things like comparison op, bitwidth, etc.)
    aux: i128,
    /// Optional name for instructions whose value depends on a
    /// symbol (currently only `GlobalAddr`). Audit Med-2: a hashed
    /// aux risked theoretical SipHash13 collisions merging two
    /// different globals into one ADRP+ADD; the explicit name is
    /// soundness-bearing, not a performance hint.
    name: Option<String>,
    /// Result type — same expression on same operands but different
    /// declared result type would be a bug, but we include it for safety.
    ty: IrType,
}

/// Build a canonical key for an instruction. Returns `None` if the
/// instruction is impure or otherwise not a CSE candidate.
fn key_of(inst: &Inst) -> Option<Key> {
    let mk = |tag: u32, ops: Vec<ValueId>, aux: i128| -> Option<Key> {
        Some(Key {
            tag,
            operands: ops,
            aux,
            name: None,
            ty: inst.ty.clone(),
        })
    };
    let mk_named = |tag: u32, name: String| -> Option<Key> {
        Some(Key {
            tag,
            operands: vec![],
            aux: 0,
            name: Some(name),
            ty: inst.ty.clone(),
        })
    };
    let canon = |a: ValueId, b: ValueId| -> Vec<ValueId> {
        if a.0 <= b.0 {
            vec![a, b]
        } else {
            vec![b, a]
        }
    };

    match &inst.kind {
        // Pure address-of-global — no operands, pure function of
        // the symbol name. Two ADRP+ADD pairs to the same global
        // inside the same block should fold. Audit Med-3 (CSE-eligible)
        // and Med-2 (collision-free key).
        InstKind::GlobalAddr(name) => mk_named(90, name.clone()),

        // Constants ------------------------------------------------------
        // Audit Min-4: width is carried by the `ty` field, so the
        // aux can just be the literal value / bit pattern.
        // Audit B-8: normalize the int value at its declared width
        // before keying so that semantically-equal constants stored
        // at different bit patterns dedupe. Example: `ConstInt(255, I8)`
        // and `ConstInt(-1, I8)` both represent -1 in i8 — keying on
        // the raw `*v` fails to dedupe them.
        InstKind::ConstInt(v, w) => {
            let bits = w.bits();
            // Sign-extend at width: low `bits` bits → i64 sign-extended.
            let signed = if bits >= 128 {
                *v
            } else {
                let shift = 128 - bits;
                (*v << shift) >> shift
            };
            mk(1, vec![], signed)
        }
        InstKind::ConstFloat(v, _) => mk(2, vec![], v.to_bits() as i128),
        InstKind::ConstBool(b) => mk(3, vec![], if *b { 1 } else { 0 }),

        // Integer arithmetic --------------------------------------------
        InstKind::IAdd(a, b) => mk(10, canon(*a, *b), 0),
        InstKind::ISub(a, b) => mk(11, vec![*a, *b], 0),
        InstKind::IMul(a, b) => mk(12, canon(*a, *b), 0),
        InstKind::IDiv(a, b) => mk(13, vec![*a, *b], 0),
        InstKind::IMod(a, b) => mk(14, vec![*a, *b], 0),
        InstKind::INeg(a) => mk(15, vec![*a], 0),

        // Float arithmetic ----------------------------------------------
        InstKind::FAdd(a, b) => mk(20, canon(*a, *b), 0),
        InstKind::FSub(a, b) => mk(21, vec![*a, *b], 0),
        InstKind::FMul(a, b) => mk(22, canon(*a, *b), 0),
        InstKind::FDiv(a, b) => mk(23, vec![*a, *b], 0),
        InstKind::FNeg(a) => mk(24, vec![*a], 0),
        InstKind::FAbs(a) => mk(25, vec![*a], 0),
        InstKind::FSqrt(a) => mk(26, vec![*a], 0),
        InstKind::FPow(a, b) => mk(27, vec![*a, *b], 0),

        // Comparisons ---------------------------------------------------
        InstKind::ICmp(op, a, b) => {
            let aux = *op as i128;
            let ops = match op {
                CmpOp::Eq | CmpOp::Ne => canon(*a, *b),
                _ => vec![*a, *b],
            };
            mk(30, ops, aux)
        }
        InstKind::FCmp(op, a, b) => {
            let aux = *op as i128;
            let ops = match op {
                CmpOp::Eq | CmpOp::Ne => canon(*a, *b),
                _ => vec![*a, *b],
            };
            mk(31, ops, aux)
        }

        // Logic ---------------------------------------------------------
        InstKind::And(a, b) => mk(40, canon(*a, *b), 0),
        InstKind::Or(a, b) => mk(41, canon(*a, *b), 0),
        InstKind::Not(a) => mk(42, vec![*a], 0),

        InstKind::Select(c, t, f) => mk(43, vec![*c, *t, *f], 0),

        // Bitwise -------------------------------------------------------
        InstKind::BitAnd(a, b) => mk(50, canon(*a, *b), 0),
        InstKind::BitOr(a, b) => mk(51, canon(*a, *b), 0),
        InstKind::BitXor(a, b) => mk(52, canon(*a, *b), 0),
        InstKind::BitNot(a) => mk(53, vec![*a], 0),
        InstKind::Shl(a, b) => mk(54, vec![*a, *b], 0),
        InstKind::LShr(a, b) => mk(55, vec![*a, *b], 0),
        InstKind::AShr(a, b) => mk(56, vec![*a, *b], 0),
        InstKind::CountLeadingZeros(a) => mk(57, vec![*a], 0),
        InstKind::CountTrailingZeros(a) => mk(58, vec![*a], 0),
        InstKind::PopCount(a) => mk(59, vec![*a], 0),

        // Conversions ---------------------------------------------------
        InstKind::IntToFloat(v, fw) => mk(60, vec![*v], fw.bits() as i128),
        InstKind::FloatToInt(v, w) => mk(61, vec![*v], w.bits() as i128),
        InstKind::FloatExtend(v, fw) => mk(62, vec![*v], fw.bits() as i128),
        InstKind::FloatTrunc(v, fw) => mk(63, vec![*v], fw.bits() as i128),
        InstKind::IntExtend(v, w, sgn) => {
            mk(64, vec![*v], (w.bits() as i128) | ((*sgn as i128) << 32))
        }
        InstKind::IntTrunc(v, w) => mk(65, vec![*v], w.bits() as i128),
        InstKind::PtrToInt(v) => mk(66, vec![*v], 0),
        InstKind::IntToPtr(v, _) => mk(67, vec![*v], 0),

        // Address arithmetic --------------------------------------------
        InstKind::GetElementPtr(base, idxs) => {
            let mut ops = vec![*base];
            ops.extend(idxs);
            mk(70, ops, 0)
        }

        // Aggregates ----------------------------------------------------
        InstKind::ExtractField(agg, i) => mk(80, vec![*agg], *i as i128),
        // InsertField produces a new aggregate value — pure but rarely
        // duplicate; include for completeness.
        InstKind::InsertField(agg, i, v) => mk(81, vec![*agg, *v], *i as i128),

        // Impure / not handled ------------------------------------------
        InstKind::Load(..)
        | InstKind::Store(..)
        | InstKind::VolatileLoad(..)
        | InstKind::VolatileStore(..)
        | InstKind::Alloca(..)
        | InstKind::Call(..)
        | InstKind::RuntimeCall(..)
        | InstKind::ConstString(..)
        | InstKind::Undef(..) => None,

        // Vector ops not yet CSE-eligible — Stage 1 lands the
        // type/instruction system; CSE keying for SIMD lands when the
        // vectorizer starts producing them.
        InstKind::VAdd(..)
        | InstKind::VSub(..)
        | InstKind::VMul(..)
        | InstKind::VDiv(..)
        | InstKind::VNeg(..)
        | InstKind::VAbs(..)
        | InstKind::VSqrt(..)
        | InstKind::VFma(..)
        | InstKind::VSelect(..)
        | InstKind::VMin(..)
        | InstKind::VMax(..)
        | InstKind::VICmp(..)
        | InstKind::VFCmp(..)
        | InstKind::VLoad(..)
        | InstKind::VStore(..)
        | InstKind::VBitcast(..)
        | InstKind::VExtract(..)
        | InstKind::VInsert(..)
        | InstKind::VBroadcast(..)
        | InstKind::VReduceSum(..)
        | InstKind::VReduceMin(..)
        | InstKind::VReduceMax(..) => None,
    }
}

/// The local CSE pass.
pub struct LocalCse;

impl Pass for LocalCse {
    fn name(&self) -> &'static str {
        "local-cse"
    }

    fn run(&self, module: &mut Module) -> bool {
        let fpenv_effects = super::fpenv::analyze_fpenv_effects(module);
        let mut changed = false;
        for (func_idx, func) in module.functions.iter_mut().enumerate() {
            // Collect all (old, new) rewrites first, then apply them
            // in a *single* function walk. Audit Min-1: the previous
            // version called `substitute_uses` once per rewrite, so a
            // function with N CSE candidates ran N full walks for an
            // overall O(N · function_size). The batched form is one
            // walk with HashMap-driven renaming.
            // In a function that accesses the floating-point environment,
            // sensitive FP ops must not be CSE'd. Their values can differ
            // across rounding-mode changes, and repeated execution can
            // re-raise a sticky IEEE status flag after a reset.
            let fpenv_barrier = fpenv_effects.may_cross_fpenv_barrier[func_idx];
            let mut rewrite_map: HashMap<ValueId, ValueId> = HashMap::new();
            for block in &func.blocks {
                let mut seen: HashMap<Key, ValueId> = HashMap::new();
                for inst in &block.insts {
                    if fpenv_barrier && super::fpenv::is_fpenv_sensitive(&inst.kind) {
                        continue;
                    }
                    let Some(k) = key_of(inst) else { continue };
                    if let Some(&first) = seen.get(&k) {
                        rewrite_map.insert(inst.id, first);
                    } else {
                        seen.insert(k, inst.id);
                    }
                }
            }
            if rewrite_map.is_empty() {
                continue;
            }

            // Audit B-7: in **local** CSE, every entry maps a later
            // duplicate to its block's *first* occurrence — and that
            // first occurrence is, by construction, never itself a
            // key in the map. So pointer chains never form, and the
            // chase loop the first version had would always exit
            // after zero iterations. Removed. If a future global
            // CSE / GVN pass reuses this map shape and CAN produce
            // chains, the chase logic will need to come back —
            // strict-decrease in ValueId guarantees termination.
            substitute_uses_batch(func, &rewrite_map);
            for block in &mut func.blocks {
                block
                    .insts
                    .retain(|inst| !rewrite_map.contains_key(&inst.id));
            }
            func.rebuild_type_cache();
            changed = true;
        }
        changed
    }
}

/// Replace every operand `old` with `rewrite_map[old]` (if any) in a
/// single walk over the function. Pairs with Min-1: avoids the
/// O(N · size) cost of calling `substitute_uses` once per rename.
/// Audit B-6: delegates to `walk::for_each_*_operand_mut`.
///
/// Audit B-9: the closure `r` captures `rewrites` by shared
/// reference and is therefore `Copy`. This is what lets us pass
/// `r` by value into multiple `for_each_operand_mut` calls inside
/// the per-block loop. If the closure ever needs mutable state
/// (e.g., to count rewrites), it stops being `Copy` and the loop
/// must shift to `&mut r` — at which point the helper signatures
/// in `walk.rs` would need to take `&mut impl FnMut(...)` instead
/// of `mut r: impl FnMut(...)`.
fn substitute_uses_batch(func: &mut Function, rewrites: &HashMap<ValueId, ValueId>) {
    let r = |v: &mut ValueId| {
        if let Some(&new) = rewrites.get(v) {
            *v = new;
        }
    };
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            for_each_operand_mut(&mut inst.kind, r);
        }
        if let Some(term) = &mut block.terminator {
            for_each_terminator_operand_mut(term, r);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{FloatWidth, FuncSig, IntWidth, IrType};
    use crate::lexer::{Position, Span};

    fn dummy_span() -> Span {
        let p = Position { line: 1, col: 1 };
        Span {
            start: p,
            end: p,
            file_id: 0,
        }
    }

    fn push(f: &mut Function, kind: InstKind, ty: IrType) -> ValueId {
        let id = f.next_value_id();
        let entry = f.entry;
        f.block_mut(entry).insts.push(Inst {
            id,
            kind,
            ty,
            span: dummy_span(),
        });
        id
    }

    #[test]
    fn dedupes_iadd_pair() {
        // %0 = const 1
        // %1 = const 2
        // %2 = iadd %0, %1
        // %3 = iadd %0, %1   ; same as %2
        // ret %3 → after CSE → ret %2 (and %3 is dead)
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let a = push(
            &mut f,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let b = push(
            &mut f,
            InstKind::ConstInt(2, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let c1 = push(&mut f, InstKind::IAdd(a, b), IrType::Int(IntWidth::I32));
        let c2 = push(&mut f, InstKind::IAdd(a, b), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(c2)));
        m.add_function(f);

        assert!(LocalCse.run(&mut m));
        // Terminator now references c1 instead of c2.
        match &m.functions[0].blocks[0].terminator {
            Some(Terminator::Return(Some(v))) => assert_eq!(*v, c1),
            _ => panic!(),
        }
        assert!(
            m.functions[0].blocks[0]
                .insts
                .iter()
                .all(|inst| inst.id != c2),
            "the rewritten duplicate must be removed"
        );
        assert!(!LocalCse.run(&mut m), "a second run must be a no-op");
    }

    #[test]
    fn commutative_iadd_dedupes_swapped_operands() {
        // %2 = iadd %0, %1
        // %3 = iadd %1, %0
        // Should canonicalize to the same key.
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let a = push(
            &mut f,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let b = push(
            &mut f,
            InstKind::ConstInt(2, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let c1 = push(&mut f, InstKind::IAdd(a, b), IrType::Int(IntWidth::I32));
        let c2 = push(&mut f, InstKind::IAdd(b, a), IrType::Int(IntWidth::I32));
        let _ = c2;
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(c2)));
        m.add_function(f);

        assert!(LocalCse.run(&mut m));
        match &m.functions[0].blocks[0].terminator {
            Some(Terminator::Return(Some(v))) => assert_eq!(*v, c1),
            _ => panic!(),
        }
    }

    #[test]
    fn non_commutative_isub_does_not_dedupe_swapped() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let a = push(
            &mut f,
            InstKind::ConstInt(5, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let b = push(
            &mut f,
            InstKind::ConstInt(3, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let _c1 = push(&mut f, InstKind::ISub(a, b), IrType::Int(IntWidth::I32));
        let c2 = push(&mut f, InstKind::ISub(b, a), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(c2)));
        m.add_function(f);

        // Returns c2 unchanged — no rewrite was possible.
        assert!(!LocalCse.run(&mut m));
    }

    #[test]
    fn keeps_load_pair_intact() {
        // Loads must NOT be deduplicated by local CSE.
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let addr = push(
            &mut f,
            InstKind::Alloca(IrType::Int(IntWidth::I32)),
            IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
        );
        let _l1 = push(&mut f, InstKind::Load(addr), IrType::Int(IntWidth::I32));
        let _l2 = push(&mut f, InstKind::Load(addr), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(!LocalCse.run(&mut m));
    }

    #[test]
    fn fmul_dedupes() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Float(FloatWidth::F64));
        let a = push(
            &mut f,
            InstKind::ConstFloat(1.5, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let b = push(
            &mut f,
            InstKind::ConstFloat(2.5, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let m1 = push(&mut f, InstKind::FMul(a, b), IrType::Float(FloatWidth::F64));
        let m2 = push(&mut f, InstKind::FMul(b, a), IrType::Float(FloatWidth::F64));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(m2)));
        m.add_function(f);

        assert!(LocalCse.run(&mut m));
        match &m.functions[0].blocks[0].terminator {
            Some(Terminator::Return(Some(v))) => assert_eq!(*v, m1),
            _ => panic!(),
        }
    }

    #[test]
    fn keeps_fp_expressions_distinct_across_indirect_call() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Float(FloatWidth::F64));
        let a = push(
            &mut f,
            InstKind::ConstFloat(1.0, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let b = push(
            &mut f,
            InstKind::ConstFloat(5.551_115_123_125_783e-17, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let target_ty = IrType::FuncPtr(Box::new(FuncSig {
            params: vec![],
            ret: IrType::Void,
        }));
        let target = push(&mut f, InstKind::Undef(target_ty.clone()), target_ty);
        let _down = push(&mut f, InstKind::FAdd(a, b), IrType::Float(FloatWidth::F64));
        push(
            &mut f,
            InstKind::Call(FuncRef::Indirect(target), vec![]),
            IrType::Void,
        );
        let up = push(&mut f, InstKind::FAdd(a, b), IrType::Float(FloatWidth::F64));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(up)));
        m.add_function(f);

        assert!(
            !LocalCse.run(&mut m),
            "an indirect call may change the rounding mode between FP expressions"
        );
    }

    #[test]
    fn keeps_fcmp_distinct_when_function_accesses_fp_environment() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Bool);
        let snan = push(
            &mut f,
            InstKind::ConstFloat(f64::from_bits(0x7ff0_0000_0000_0001), FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let zero = push(
            &mut f,
            InstKind::ConstFloat(0.0, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let first = push(&mut f, InstKind::FCmp(CmpOp::Eq, snan, zero), IrType::Bool);
        let invalid = push(
            &mut f,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let clear = push(
            &mut f,
            InstKind::ConstInt(0, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        push(
            &mut f,
            InstKind::Call(
                FuncRef::External("afs_ieee_set_flag".into()),
                vec![invalid, clear],
            ),
            IrType::Void,
        );
        let second = push(&mut f, InstKind::FCmp(CmpOp::Eq, snan, zero), IrType::Bool);
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(second)));
        m.add_function(f);

        assert!(
            !LocalCse.run(&mut m),
            "the second signaling comparison must execute after IEEE_INVALID is cleared"
        );
        let comparisons = m.functions[0].blocks[0]
            .insts
            .iter()
            .filter(|inst| matches!(inst.kind, InstKind::FCmp(..)))
            .map(|inst| inst.id)
            .collect::<Vec<_>>();
        assert_eq!(comparisons, vec![first, second]);
        assert!(matches!(
            m.functions[0].blocks[0].terminator,
            Some(Terminator::Return(Some(value))) if value == second
        ));
    }

    #[test]
    fn fcmp_still_dedupes_without_fp_environment_access() {
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Bool);
        let value = push(
            &mut f,
            InstKind::ConstFloat(1.0, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let zero = push(
            &mut f,
            InstKind::ConstFloat(0.0, FloatWidth::F64),
            IrType::Float(FloatWidth::F64),
        );
        let first = push(&mut f, InstKind::FCmp(CmpOp::Gt, value, zero), IrType::Bool);
        let second = push(&mut f, InstKind::FCmp(CmpOp::Gt, value, zero), IrType::Bool);
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(second)));
        m.add_function(f);

        assert!(LocalCse.run(&mut m));
        assert!(matches!(
            m.functions[0].blocks[0].terminator,
            Some(Terminator::Return(Some(value))) if value == first
        ));
        assert!(
            m.functions[0].blocks[0]
                .insts
                .iter()
                .all(|inst| inst.id != second),
            "ordinary comparison CSE must remain enabled without an FP-environment barrier"
        );
    }

    #[test]
    fn icmp_lt_not_canonicalized() {
        // Lt is not commutative — must not collapse.
        let mut m = Module::new("t".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Bool);
        let a = push(
            &mut f,
            InstKind::ConstInt(1, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let b = push(
            &mut f,
            InstKind::ConstInt(2, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let _c1 = push(&mut f, InstKind::ICmp(CmpOp::Lt, a, b), IrType::Bool);
        let c2 = push(&mut f, InstKind::ICmp(CmpOp::Lt, b, a), IrType::Bool);
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(c2)));
        m.add_function(f);

        assert!(!LocalCse.run(&mut m));
    }
}
