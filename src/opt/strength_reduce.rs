//! Strength reduction.
//!
//! Replaces expensive integer operations with cheaper equivalents:
//!
//!  * `imul x, 2^k`  →  `shl x, k`
//!  * `imul x, 0`    →  `const(0)`
//!  * `imul x, 1`    →  identity (rewrite uses)
//!  * `imul x, -1`   →  `ineg x`
//!  * `idiv x, 1`    →  identity
//!  * `idiv x, -1`   →  `ineg x`
//!  * `iadd x, 0`    →  identity
//!  * `isub x, 0`    →  identity
//!  * `isub 0, x`    →  `ineg x`
//!  * `bit_and x, 0` →  `const(0)`
//!  * `bit_and x, -1` → identity
//!  * `bit_or x, 0`  →  identity
//!  * `bit_or x, -1` →  `const(-1)`
//!  * `bit_xor x, 0` →  identity
//!  * `shl/lshr/ashr x, 0` → identity
//!
//! ## What we deliberately don't do here
//!
//!  * **Signed division by `2^k`**: a single `ashr` is wrong because
//!    integer division rounds *toward zero* while arithmetic shift
//!    rounds *toward -infinity* for negatives. The correct lowering
//!    needs a sign-bias adjustment (`(x + ((x >> 63) & ((1<<k)-1))) >> k`)
//!    which is three instructions, not one — still profitable, but it
//!    grows the IR. We'll handle it in a dedicated "div by constant"
//!    lowering once we have a benchmark to point at.
//!
//!  * **Signed modulo by `2^k`**: same problem — signed-mod semantics
//!    differ from `bit_and` for negative dividends. Skip.
//!
//!  * **Floating-point identities**: `x + 0.0`, `x * 1.0`, etc. are
//!    *not* IEEE-equivalent (signed zeros, signaling NaNs). They're
//!    only safe under `-Ofast` / fast-math, which we don't yet have.
//!
//! ## Implementation
//!
//! The pass walks every block and rewrites instructions in place where
//! the result type is identical to the rewrite. For "identity"
//! rewrites that pass a value through unchanged, we use
//! `util::substitute_uses` to retarget downstream consumers at the
//! original operand and let DCE clean up the now-dead instruction.

use super::pass::Pass;
use super::util::{for_each_operand_mut, for_each_terminator_operand_mut};
use crate::ir::inst::*;
use crate::ir::types::{IrType, IntWidth};
use std::collections::HashMap;

/// Apply `rewrites` to every operand slot in the function in a
/// single walk. Audit B-2: replaces the previous per-Identity
/// `substitute_uses` calls (which each walked the function in
/// O(function_size)) with a single O(function_size) walk for any
/// number of rewrites.
///
/// Audit B-9: see `cse::substitute_uses_batch` for the closure
/// `Copy`-by-capture contract — same constraint applies here.
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

/// Look up the constant integer value behind a `ValueId`, if any.
fn const_int(consts: &HashMap<ValueId, (i64, IntWidth)>, id: ValueId) -> Option<(i64, IntWidth)> {
    consts.get(&id).copied()
}

/// Collect integer constants in canonical form (sign-extended at
/// their declared width). Audit m4-1: this mirrors the N-9 / M4-4
/// normalization in `const_fold` and `const_prop`. The previous
/// version stored raw bit patterns, so `rewrite_for` had to re-sext
/// at use-site — and did so using the *result* type's width, not the
/// operand's, producing conservative no-ops on width-drifted inputs.
fn collect_int_consts(func: &Function) -> HashMap<ValueId, (i64, IntWidth)> {
    let mut consts = HashMap::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if let InstKind::ConstInt(v, w) = inst.kind {
                consts.insert(inst.id, (sext(v, w.bits()), w));
            }
        }
    }
    consts
}

/// Sign-extend a stored i64 to its declared bit width. Used so that
/// "is this -1?" checks survive narrow types.
fn sext(v: i64, bits: u32) -> i64 {
    if bits >= 64 { v }
    else {
        let shift = 64 - bits;
        (v << shift) >> shift
    }
}

/// Power-of-two test that returns the exponent. Returns `Some(k)` only
/// when `v == 1 << k` and `k > 0`. (`v == 1` is handled separately as
/// "multiply by 1 → identity".)
fn pow2_exponent(v: i64) -> Option<u32> {
    if v <= 0 { return None; }
    let u = v as u64;
    if u.is_power_of_two() && u != 1 {
        Some(u.trailing_zeros())
    } else {
        None
    }
}

/// One rewrite directive describing how a single instruction should be
/// replaced. Computed during the read-only walk and applied afterwards
/// so we don't fight the borrow checker.
enum Rewrite {
    /// Replace this instruction's `kind` with a new one.
    KindOnly(InstKind),
    /// Replace this instruction's `kind` AND retarget every use of the
    /// old result `ValueId` to `pass_through`. Used for identity ops.
    Identity { pass_through: ValueId },
    /// Rewrite as `shl var, const(k)`. The apply phase synthesizes a
    /// fresh `ConstInt(k)` instruction and inserts it directly before
    /// the rewritten instruction.
    ShlByConst { var: ValueId, k: u32 },
}

/// Try to derive a strength-reduction rewrite for one instruction.
/// Returns `None` if no rewrite applies.
fn rewrite_for(
    inst: &Inst,
    consts: &HashMap<ValueId, (i64, IntWidth)>,
) -> Option<Rewrite> {
    let int_w = if let IrType::Int(w) = inst.ty { w } else { return None; };
    // Audit m4-1: after the M4-4 normalization in `collect_int_consts`,
    // every `kv` returned from `const_int` is already sign-extended
    // at its source width. The earlier code re-sext'd at `int_w` (the
    // *result* width), which was redundant at best and actively
    // harmful when an operand's source width differed from the result
    // width (the re-sext would truncate a wider value to a narrower
    // signed range, making e.g. `imul x, 2000000000` at result-type
    // i8 look like `imul x, 0` and spuriously rewrite to const(0)).
    // Now we just compare against the canonical i64-stored value.

    match &inst.kind {
        // ---- imul -------------------------------------------------------
        InstKind::IMul(a, b) => {
            let var = if let Some((kv, _)) = const_int(consts, *b) {
                Some((*a, kv))
            } else if let Some((kv, _)) = const_int(consts, *a) {
                Some((*b, kv))
            } else {
                None
            };
            if let Some((var_id, kv)) = var {
                if kv == 0 {
                    return Some(Rewrite::KindOnly(InstKind::ConstInt(0, int_w)));
                }
                if kv == 1 {
                    return Some(Rewrite::Identity { pass_through: var_id });
                }
                if kv == -1 {
                    return Some(Rewrite::KindOnly(InstKind::INeg(var_id)));
                }
                if let Some(k) = pow2_exponent(kv) {
                    return Some(Rewrite::ShlByConst { var: var_id, k });
                }
            }
            None
        }
        // ---- iadd -------------------------------------------------------
        InstKind::IAdd(a, b) => {
            if let Some((0, _)) = const_int(consts, *b) {
                return Some(Rewrite::Identity { pass_through: *a });
            }
            if let Some((0, _)) = const_int(consts, *a) {
                return Some(Rewrite::Identity { pass_through: *b });
            }
            None
        }
        // ---- isub -------------------------------------------------------
        InstKind::ISub(a, b) => {
            if let Some((0, _)) = const_int(consts, *b) {
                return Some(Rewrite::Identity { pass_through: *a });
            }
            if let Some((0, _)) = const_int(consts, *a) {
                return Some(Rewrite::KindOnly(InstKind::INeg(*b)));
            }
            None
        }
        // ---- idiv -------------------------------------------------------
        InstKind::IDiv(a, b) => {
            if let Some((kv, _)) = const_int(consts, *b) {
                if kv == 1 {
                    return Some(Rewrite::Identity { pass_through: *a });
                }
                if kv == -1 {
                    return Some(Rewrite::KindOnly(InstKind::INeg(*a)));
                }
                // Power-of-two signed division skipped (see module doc).
            }
            None
        }
        // ---- bit_and ---------------------------------------------------
        InstKind::BitAnd(a, b) => {
            if let Some((kv, _)) = const_int(consts, *b) {
                if kv == 0 {
                    return Some(Rewrite::KindOnly(InstKind::ConstInt(0, int_w)));
                }
                if kv == -1 {
                    return Some(Rewrite::Identity { pass_through: *a });
                }
            }
            if let Some((kv, _)) = const_int(consts, *a) {
                if kv == 0 {
                    return Some(Rewrite::KindOnly(InstKind::ConstInt(0, int_w)));
                }
                if kv == -1 {
                    return Some(Rewrite::Identity { pass_through: *b });
                }
            }
            None
        }
        // ---- bit_or ----------------------------------------------------
        InstKind::BitOr(a, b) => {
            if let Some((kv, _)) = const_int(consts, *b) {
                if kv == 0 {
                    return Some(Rewrite::Identity { pass_through: *a });
                }
                if kv == -1 {
                    return Some(Rewrite::KindOnly(InstKind::ConstInt(-1, int_w)));
                }
            }
            if let Some((kv, _)) = const_int(consts, *a) {
                if kv == 0 {
                    return Some(Rewrite::Identity { pass_through: *b });
                }
                if kv == -1 {
                    return Some(Rewrite::KindOnly(InstKind::ConstInt(-1, int_w)));
                }
            }
            None
        }
        // ---- bit_xor ---------------------------------------------------
        InstKind::BitXor(a, b) => {
            if let Some((0, _)) = const_int(consts, *b) {
                return Some(Rewrite::Identity { pass_through: *a });
            }
            if let Some((0, _)) = const_int(consts, *a) {
                return Some(Rewrite::Identity { pass_through: *b });
            }
            None
        }
        // ---- shifts ----------------------------------------------------
        InstKind::Shl(a, b) | InstKind::LShr(a, b) | InstKind::AShr(a, b) => {
            if let Some((kv, _)) = const_int(consts, *b) {
                if kv == 0 {
                    return Some(Rewrite::Identity { pass_through: *a });
                }
            }
            None
        }
        _ => None,
    }
}

pub struct StrengthReduce;

impl Pass for StrengthReduce {
    fn name(&self) -> &'static str { "strength-reduce" }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            // Snapshot constants once per function. Strength reduction
            // never produces a *new* constant operand whose appearance
            // would unlock further reductions on the same pass; if it
            // did, the manager would re-run us anyway.
            let consts = collect_int_consts(func);

            // First pass: collect (block_idx, inst_idx, rewrite) tuples.
            let mut rewrites: Vec<(usize, usize, Rewrite, ValueId, IrType)> = Vec::new();
            for (bi, block) in func.blocks.iter().enumerate() {
                for (ii, inst) in block.insts.iter().enumerate() {
                    if let Some(rw) = rewrite_for(inst, &consts) {
                        rewrites.push((bi, ii, rw, inst.id, inst.ty.clone()));
                    }
                }
            }
            if rewrites.is_empty() { continue; }

            // Second pass: apply rewrites. We process them in reverse
            // block-then-instruction order so that inserting a new
            // const instruction at index `ii` doesn't shift the
            // indices of any earlier rewrite still pending.
            rewrites.sort_by(|a, b| (b.0, b.1).cmp(&(a.0, a.1)));

            // Audit B-2: collect all Identity rewrites into a single
            // rename map and apply them in one function walk at the
            // end, instead of calling `substitute_uses` once per
            // Identity rewrite (which would re-walk the function for
            // each rewrite). The CSE pass got the same treatment in
            // the Min-1 fix; this finally closes the parallel.
            let mut identity_rewrites: HashMap<ValueId, ValueId> = HashMap::new();

            for (bi, ii, rw, old_id, ty) in rewrites {
                match rw {
                    Rewrite::ShlByConst { var, k } => {
                        let int_w = if let IrType::Int(w) = ty { w } else { unreachable!() };
                        let kid = func.next_value_id();
                        let span = func.blocks[bi].insts[ii].span;
                        func.blocks[bi].insts.insert(ii, Inst {
                            id: kid,
                            kind: InstKind::ConstInt(k as i64, int_w),
                            ty: IrType::Int(int_w),
                            span,
                        });
                        // The original instruction shifted to ii+1.
                        func.blocks[bi].insts[ii + 1].kind = InstKind::Shl(var, kid);
                    }
                    Rewrite::KindOnly(new_kind) => {
                        func.blocks[bi].insts[ii].kind = new_kind;
                    }
                    Rewrite::Identity { pass_through } => {
                        // Defer the substitution. Record the
                        // (old_id → pass_through) pair and apply all
                        // such pairs in one walk after this loop.
                        // The orphan instruction is left alone so
                        // dominance still holds; DCE removes it on
                        // the next sweep.
                        //
                        // Audit M-A1 / C-A: the original code wrote a
                        // placeholder `Const(0)` here, then `continue`d
                        // for non-int/bool types — silently swallowing
                        // `changed = true` after mutating the function.
                        // The structural fix removed the placeholder.
                        identity_rewrites.insert(old_id, pass_through);
                        let _ = ty; // intentionally unused
                    }
                }
                changed = true;
            }

            // Apply all Identity rewrites in a single function walk.
            // Chase pointer chains so each entry maps directly to its
            // terminal target — strength_reduce can produce chains
            // like `((x*1)*1)+0` where each Identity points to the
            // result of an earlier Identity.
            //
            // Audit N-7: termination is guaranteed today by the SSA
            // invariant that every Identity rewrite points to a
            // value with a strictly smaller `ValueId` (operands are
            // defined before their uses). But that invariant isn't
            // enforced anywhere — a future pass could in principle
            // produce a cycle. We defend with a `HashSet<ValueId>`
            // visited guard that breaks the chase on the second
            // visit of any node, so a bug-introduced cycle becomes
            // "this rewrite is skipped" rather than "compiler hangs
            // in an infinite loop."
            if !identity_rewrites.is_empty() {
                let keys: Vec<ValueId> = identity_rewrites.keys().copied().collect();
                for k in keys {
                    let mut cur = identity_rewrites[&k];
                    let mut visited: std::collections::HashSet<ValueId> =
                        std::collections::HashSet::new();
                    visited.insert(k);
                    while let Some(&next) = identity_rewrites.get(&cur) {
                        if !visited.insert(next) { break; }
                        cur = next;
                    }
                    identity_rewrites.insert(k, cur);
                }
                substitute_uses_batch(func, &identity_rewrites);
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::IrType;
    use crate::lexer::{Span, Position};

    fn dummy_span() -> Span {
        let p = Position { line: 1, col: 1 };
        Span { start: p, end: p, file_id: 0 }
    }

    fn push(f: &mut Function, kind: InstKind, ty: IrType) -> ValueId {
        let id = f.next_value_id();
        let entry = f.entry;
        f.block_mut(entry).insts.push(Inst { id, kind, ty, span: dummy_span() });
        id
    }

    fn make_fn() -> (Module, Function) {
        let m = Module::new("t".into());
        let f = Function::new("f".into(), vec![], IrType::Void);
        (m, f)
    }

    #[test]
    fn imul_by_pow2_becomes_shl() {
        let (mut m, mut f) = make_fn();
        let x = push(&mut f, InstKind::ConstInt(7, IntWidth::I32), IrType::Int(IntWidth::I32));
        let k = push(&mut f, InstKind::ConstInt(8, IntWidth::I32), IrType::Int(IntWidth::I32));
        let y = push(&mut f, InstKind::IMul(x, k), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(y)));
        m.add_function(f);

        assert!(StrengthReduce.run(&mut m));
        // The Shl should reference x and a freshly inserted const(3).
        let block = &m.functions[0].blocks[0];
        let shl = block.insts.iter().find_map(|i| match &i.kind {
            InstKind::Shl(var, kid) => Some((*var, *kid)),
            _ => None,
        }).expect("expected Shl");
        assert_eq!(shl.0, x);
        // The shift count should be a const(3).
        let kconst = block.insts.iter().find(|i| i.id == shl.1).expect("shift const");
        assert!(matches!(kconst.kind, InstKind::ConstInt(3, IntWidth::I32)));
    }

    #[test]
    fn imul_by_zero_becomes_const_zero() {
        let (mut m, mut f) = make_fn();
        let x = push(&mut f, InstKind::ConstInt(7, IntWidth::I32), IrType::Int(IntWidth::I32));
        let z = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));
        let y = push(&mut f, InstKind::IMul(x, z), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(y)));
        m.add_function(f);

        assert!(StrengthReduce.run(&mut m));
        // y now defines const(0)
        let y_inst = m.functions[0].blocks[0].insts.iter().find(|i| i.id == y).unwrap();
        assert!(matches!(y_inst.kind, InstKind::ConstInt(0, IntWidth::I32)));
    }

    #[test]
    fn imul_by_one_becomes_identity() {
        let (mut m, mut f) = make_fn();
        let x = push(&mut f, InstKind::ConstInt(7, IntWidth::I32), IrType::Int(IntWidth::I32));
        let one = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
        let y = push(&mut f, InstKind::IMul(x, one), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(y)));
        m.add_function(f);

        assert!(StrengthReduce.run(&mut m));
        // Return now uses x directly.
        match &m.functions[0].blocks[0].terminator {
            Some(Terminator::Return(Some(v))) => assert_eq!(*v, x),
            other => panic!("expected Return(x), got {:?}", other),
        }
    }

    #[test]
    fn imul_by_neg_one_becomes_neg() {
        let (mut m, mut f) = make_fn();
        let x = push(&mut f, InstKind::ConstInt(7, IntWidth::I32), IrType::Int(IntWidth::I32));
        let neg = push(&mut f, InstKind::ConstInt(-1, IntWidth::I32), IrType::Int(IntWidth::I32));
        let y = push(&mut f, InstKind::IMul(x, neg), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(y)));
        m.add_function(f);

        assert!(StrengthReduce.run(&mut m));
        let y_inst = m.functions[0].blocks[0].insts.iter().find(|i| i.id == y).unwrap();
        match y_inst.kind {
            InstKind::INeg(v) => assert_eq!(v, x),
            ref other => panic!("expected INeg, got {:?}", other),
        }
    }

    #[test]
    fn iadd_zero_becomes_identity() {
        let (mut m, mut f) = make_fn();
        let x = push(&mut f, InstKind::ConstInt(42, IntWidth::I32), IrType::Int(IntWidth::I32));
        let z = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));
        let y = push(&mut f, InstKind::IAdd(x, z), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(y)));
        m.add_function(f);

        assert!(StrengthReduce.run(&mut m));
        match &m.functions[0].blocks[0].terminator {
            Some(Terminator::Return(Some(v))) => assert_eq!(*v, x),
            _ => panic!(),
        }
    }

    #[test]
    fn isub_from_zero_becomes_neg() {
        let (mut m, mut f) = make_fn();
        let z = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));
        let x = push(&mut f, InstKind::ConstInt(42, IntWidth::I32), IrType::Int(IntWidth::I32));
        let y = push(&mut f, InstKind::ISub(z, x), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(y)));
        m.add_function(f);

        assert!(StrengthReduce.run(&mut m));
        let y_inst = m.functions[0].blocks[0].insts.iter().find(|i| i.id == y).unwrap();
        match y_inst.kind {
            InstKind::INeg(v) => assert_eq!(v, x),
            _ => panic!(),
        }
    }

    #[test]
    fn idiv_by_one_becomes_identity() {
        let (mut m, mut f) = make_fn();
        let x = push(&mut f, InstKind::ConstInt(42, IntWidth::I32), IrType::Int(IntWidth::I32));
        let one = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
        let y = push(&mut f, InstKind::IDiv(x, one), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(y)));
        m.add_function(f);

        assert!(StrengthReduce.run(&mut m));
        match &m.functions[0].blocks[0].terminator {
            Some(Terminator::Return(Some(v))) => assert_eq!(*v, x),
            _ => panic!(),
        }
    }

    #[test]
    fn band_with_neg_one_becomes_identity() {
        let (mut m, mut f) = make_fn();
        let x = push(&mut f, InstKind::ConstInt(42, IntWidth::I32), IrType::Int(IntWidth::I32));
        let mask = push(&mut f, InstKind::ConstInt(-1, IntWidth::I32), IrType::Int(IntWidth::I32));
        let y = push(&mut f, InstKind::BitAnd(x, mask), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(y)));
        m.add_function(f);

        assert!(StrengthReduce.run(&mut m));
        match &m.functions[0].blocks[0].terminator {
            Some(Terminator::Return(Some(v))) => assert_eq!(*v, x),
            _ => panic!(),
        }
    }

    #[test]
    fn band_with_zero_becomes_const_zero() {
        let (mut m, mut f) = make_fn();
        let x = push(&mut f, InstKind::ConstInt(42, IntWidth::I32), IrType::Int(IntWidth::I32));
        let z = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));
        let y = push(&mut f, InstKind::BitAnd(x, z), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(y)));
        m.add_function(f);

        assert!(StrengthReduce.run(&mut m));
        let y_inst = m.functions[0].blocks[0].insts.iter().find(|i| i.id == y).unwrap();
        assert!(matches!(y_inst.kind, InstKind::ConstInt(0, IntWidth::I32)));
    }

    #[test]
    fn shift_by_zero_becomes_identity() {
        let (mut m, mut f) = make_fn();
        let x = push(&mut f, InstKind::ConstInt(42, IntWidth::I32), IrType::Int(IntWidth::I32));
        let z = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));
        let y = push(&mut f, InstKind::Shl(x, z), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(y)));
        m.add_function(f);

        assert!(StrengthReduce.run(&mut m));
        match &m.functions[0].blocks[0].terminator {
            Some(Terminator::Return(Some(v))) => assert_eq!(*v, x),
            _ => panic!(),
        }
    }

    #[test]
    fn no_rewrite_when_no_constant_operand() {
        // imul of two non-const values: should not change.
        let mut m = Module::new("t".into());
        let params = vec![
            Param {
                name: "a".into(),
                ty: IrType::Int(IntWidth::I32),
                id: ValueId(0),
                fortran_noalias: false,
            },
            Param {
                name: "b".into(),
                ty: IrType::Int(IntWidth::I32),
                id: ValueId(1),
                fortran_noalias: false,
            },
        ];
        let mut f = Function::new("f".into(), params, IrType::Int(IntWidth::I32));
        let y = push(&mut f, InstKind::IMul(ValueId(0), ValueId(1)), IrType::Int(IntWidth::I32));
        let entry = f.entry;
        f.block_mut(entry).terminator = Some(Terminator::Return(Some(y)));
        m.add_function(f);

        assert!(!StrengthReduce.run(&mut m));
    }
}
