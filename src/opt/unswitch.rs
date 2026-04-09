//! Loop unswitching pass.
//!
//! Hoists loop-invariant conditionals out of loops by cloning the loop
//! into two versions — one for the true branch, one for the false
//! branch. Each clone has the internal conditional replaced with an
//! unconditional branch, eliminating the branch from the hot path.
//!
//! ```text
//! Before:
//!   do i = 1, n
//!     if (flag) then      ← flag is loop-invariant
//!       a(i) = b(i)
//!     else
//!       a(i) = c(i)
//!     end if
//!   end do
//!
//! After:
//!   if (flag) then
//!     do i = 1, n; a(i) = b(i); end do
//!   else
//!     do i = 1, n; a(i) = c(i); end do
//!   end if
//! ```
//!
//! Gated on body size (≤ UNSWITCH_MAX_BODY instructions) to prevent
//! code bloat. Fires at O2+.

use std::collections::{HashMap, HashSet};
use crate::ir::inst::*;
use crate::ir::walk::{find_natural_loops, predecessors, inst_uses, prune_unreachable};
use super::loop_utils::{find_preheader, loop_defined_values};
use super::pass::Pass;

/// Maximum number of instructions in the loop body to consider for
/// unswitching. Unswitching doubles the code, so keep this tight.
const UNSWITCH_MAX_BODY: usize = 50;

pub struct LoopUnswitch;

impl Pass for LoopUnswitch {
    fn name(&self) -> &'static str { "loop-unswitch" }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if unswitch_in_function(func) { changed = true; }
        }
        changed
    }
}

/// Attempt to unswitch one loop in the function. Returns true if a
/// transformation was applied. We process one loop per call and let
/// the pass manager's fixpoint loop handle cascading opportunities.
fn unswitch_in_function(func: &mut Function) -> bool {
    let loops = find_natural_loops(func);
    let preds = predecessors(func);

    for lp in &loops {
        let Some(ph_id) = find_preheader(func, lp, &preds) else { continue };

        // Size guard.
        let total_insts: usize = lp.body.iter()
            .map(|&b| func.block(b).insts.len())
            .sum();
        if total_insts > UNSWITCH_MAX_BODY { continue; }

        // Find a CondBranch inside the loop whose condition is invariant.
        let loop_defs = loop_defined_values(func, lp);

        let candidate = find_unswitch_candidate(func, lp, &loop_defs);
        let Some((cond_block, cond_val, true_dest, true_args, false_dest, false_args)) = candidate else {
            continue;
        };

        // Both successors must be inside the loop (otherwise it's the
        // loop exit condition, not an unswitchable interior branch).
        if !lp.body.contains(&true_dest) || !lp.body.contains(&false_dest) {
            continue;
        }

        // Clone the entire loop body into two copies.
        let (true_map, true_blocks) = clone_loop(func, lp);
        let (false_map, false_blocks) = clone_loop(func, lp);

        // In the true clone: replace the CondBranch with an unconditional
        // branch to the true successor.
        let true_cond_block = true_map[&cond_block];
        let true_true_dest = true_map[&true_dest];
        let remapped_true_args: Vec<ValueId> = true_args.iter()
            .map(|v| *true_map.get(&BlockId(v.0)).and_then(|_| None).unwrap_or(v))
            .collect();
        // Remap value args through the clone map.
        let true_val_map = build_value_map(func, lp, &true_blocks, &true_map);
        let remapped_true_args: Vec<ValueId> = true_args.iter()
            .map(|v| *true_val_map.get(v).unwrap_or(v))
            .collect();
        func.block_mut(true_cond_block).terminator =
            Some(Terminator::Branch(true_true_dest, remapped_true_args));

        // In the false clone: replace the CondBranch with an unconditional
        // branch to the false successor.
        let false_cond_block = false_map[&cond_block];
        let false_false_dest = false_map[&false_dest];
        let false_val_map = build_value_map(func, lp, &false_blocks, &false_map);
        let remapped_false_args: Vec<ValueId> = false_args.iter()
            .map(|v| *false_val_map.get(v).unwrap_or(v))
            .collect();
        func.block_mut(false_cond_block).terminator =
            Some(Terminator::Branch(false_false_dest, remapped_false_args));

        // Rewrite the preheader to test the condition and branch to
        // the appropriate clone's header.
        let true_header = true_map[&lp.header];
        let false_header = false_map[&lp.header];

        // Get the preheader's original branch args to the header.
        let ph_args = match &func.block(ph_id).terminator {
            Some(Terminator::Branch(_, args)) => args.clone(),
            _ => vec![],
        };

        func.block_mut(ph_id).terminator = Some(Terminator::CondBranch {
            cond: cond_val,
            true_dest: true_header,
            true_args: ph_args.clone(),
            false_dest: false_header,
            false_args: ph_args,
        });

        // Mark original loop blocks as unreachable so prune removes them.
        for &bid in &lp.body {
            func.block_mut(bid).terminator = Some(Terminator::Unreachable);
        }
        prune_unreachable(func);

        return true; // one at a time
    }
    false
}

/// Find a CondBranch in the loop body whose condition is loop-invariant.
fn find_unswitch_candidate(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    loop_defs: &HashSet<ValueId>,
) -> Option<(BlockId, ValueId, BlockId, Vec<ValueId>, BlockId, Vec<ValueId>)> {
    for &bid in &lp.body {
        let block = func.block(bid);
        if let Some(Terminator::CondBranch {
            cond, true_dest, true_args, false_dest, false_args,
        }) = &block.terminator {
            // Condition must be loop-invariant (not defined in the loop).
            if !loop_defs.contains(cond) {
                // Both targets must be in the loop (not the loop exit).
                if lp.body.contains(true_dest) && lp.body.contains(false_dest) {
                    return Some((bid, *cond, *true_dest, true_args.clone(),
                                 *false_dest, false_args.clone()));
                }
            }
        }
    }
    None
}

/// Clone all blocks in a loop body, returning a block-ID mapping
/// (old → new) and the list of new block IDs.
fn clone_loop(
    func: &mut Function,
    lp: &crate::ir::walk::NaturalLoop,
) -> (HashMap<BlockId, BlockId>, Vec<BlockId>) {
    let mut block_map: HashMap<BlockId, BlockId> = HashMap::new();
    let mut new_blocks = Vec::new();

    // First pass: create empty clone blocks and map IDs.
    let body_sorted: Vec<BlockId> = {
        let mut v: Vec<BlockId> = lp.body.iter().copied().collect();
        v.sort_by_key(|b| b.0);
        v
    };
    for &old_id in &body_sorted {
        let old_name = &func.block(old_id).name;
        let new_id = func.create_block(&format!("{}_clone", old_name));
        block_map.insert(old_id, new_id);
        new_blocks.push(new_id);
    }

    // Build value map: old block params → new block params, old insts → new insts.
    let mut val_map: HashMap<ValueId, ValueId> = HashMap::new();

    // Second pass: clone block params.
    for &old_id in &body_sorted {
        let new_id = block_map[&old_id];
        let old_params: Vec<BlockParam> = func.block(old_id).params.clone();
        for bp in &old_params {
            let new_vid = func.next_value_id();
            func.register_type(new_vid, bp.ty.clone());
            val_map.insert(bp.id, new_vid);
            func.block_mut(new_id).params.push(BlockParam {
                id: new_vid,
                ty: bp.ty.clone(),
            });
        }
    }

    // Third pass: clone instructions with remapped operands.
    for &old_id in &body_sorted {
        let new_id = block_map[&old_id];
        let old_insts: Vec<Inst> = func.block(old_id).insts.clone();
        for inst in &old_insts {
            let new_vid = func.next_value_id();
            func.register_type(new_vid, inst.ty.clone());
            val_map.insert(inst.id, new_vid);
            let new_kind = remap_inst_kind(&inst.kind, &val_map);
            func.block_mut(new_id).insts.push(Inst {
                id: new_vid,
                kind: new_kind,
                ty: inst.ty.clone(),
                span: inst.span,
            });
        }
    }

    // Fourth pass: clone terminators with remapped targets and values.
    for &old_id in &body_sorted {
        let new_id = block_map[&old_id];
        let old_term = func.block(old_id).terminator.clone();
        if let Some(term) = old_term {
            let new_term = remap_terminator(&term, &block_map, &val_map);
            func.block_mut(new_id).terminator = Some(new_term);
        }
    }

    (block_map, new_blocks)
}

/// Build a combined value map from original loop → cloned loop.
fn build_value_map(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    _new_blocks: &[BlockId],
    block_map: &HashMap<BlockId, BlockId>,
) -> HashMap<ValueId, ValueId> {
    let mut val_map: HashMap<ValueId, ValueId> = HashMap::new();
    let body_sorted: Vec<BlockId> = {
        let mut v: Vec<BlockId> = lp.body.iter().copied().collect();
        v.sort_by_key(|b| b.0);
        v
    };
    for &old_id in &body_sorted {
        let new_id = block_map[&old_id];
        let old_block = func.block(old_id);
        let new_block = func.block(new_id);
        for (old_bp, new_bp) in old_block.params.iter().zip(new_block.params.iter()) {
            val_map.insert(old_bp.id, new_bp.id);
        }
        for (old_inst, new_inst) in old_block.insts.iter().zip(new_block.insts.iter()) {
            val_map.insert(old_inst.id, new_inst.id);
        }
    }
    val_map
}

/// Remap all ValueId operands in an InstKind using the value map.
/// Values not in the map are left unchanged (they're defined outside the loop).
fn remap_inst_kind(kind: &InstKind, map: &HashMap<ValueId, ValueId>) -> InstKind {
    // Reuse the exhaustive remapper from unroll.rs's pattern.
    let r = |v: &ValueId| *map.get(v).unwrap_or(v);
    match kind {
        InstKind::ConstInt(v, w)     => InstKind::ConstInt(*v, *w),
        InstKind::ConstFloat(v, w)   => InstKind::ConstFloat(*v, *w),
        InstKind::ConstBool(v)       => InstKind::ConstBool(*v),
        InstKind::ConstString(v)     => InstKind::ConstString(v.clone()),
        InstKind::Undef(t)           => InstKind::Undef(t.clone()),
        InstKind::GlobalAddr(s)      => InstKind::GlobalAddr(s.clone()),
        InstKind::IAdd(a, b)  => InstKind::IAdd(r(a), r(b)),
        InstKind::ISub(a, b)  => InstKind::ISub(r(a), r(b)),
        InstKind::IMul(a, b)  => InstKind::IMul(r(a), r(b)),
        InstKind::IDiv(a, b)  => InstKind::IDiv(r(a), r(b)),
        InstKind::IMod(a, b)  => InstKind::IMod(r(a), r(b)),
        InstKind::INeg(a)     => InstKind::INeg(r(a)),
        InstKind::FAdd(a, b)  => InstKind::FAdd(r(a), r(b)),
        InstKind::FSub(a, b)  => InstKind::FSub(r(a), r(b)),
        InstKind::FMul(a, b)  => InstKind::FMul(r(a), r(b)),
        InstKind::FDiv(a, b)  => InstKind::FDiv(r(a), r(b)),
        InstKind::FNeg(a)     => InstKind::FNeg(r(a)),
        InstKind::FAbs(a)     => InstKind::FAbs(r(a)),
        InstKind::FSqrt(a)    => InstKind::FSqrt(r(a)),
        InstKind::FPow(a, b)  => InstKind::FPow(r(a), r(b)),
        InstKind::ICmp(op, a, b) => InstKind::ICmp(*op, r(a), r(b)),
        InstKind::FCmp(op, a, b) => InstKind::FCmp(*op, r(a), r(b)),
        InstKind::And(a, b) => InstKind::And(r(a), r(b)),
        InstKind::Or(a, b)  => InstKind::Or(r(a), r(b)),
        InstKind::Not(a)    => InstKind::Not(r(a)),
        InstKind::Select(c, t, f) => InstKind::Select(r(c), r(t), r(f)),
        InstKind::BitAnd(a, b)           => InstKind::BitAnd(r(a), r(b)),
        InstKind::BitOr(a, b)            => InstKind::BitOr(r(a), r(b)),
        InstKind::BitXor(a, b)           => InstKind::BitXor(r(a), r(b)),
        InstKind::BitNot(a)              => InstKind::BitNot(r(a)),
        InstKind::Shl(a, b)              => InstKind::Shl(r(a), r(b)),
        InstKind::LShr(a, b)             => InstKind::LShr(r(a), r(b)),
        InstKind::AShr(a, b)             => InstKind::AShr(r(a), r(b)),
        InstKind::CountLeadingZeros(a)   => InstKind::CountLeadingZeros(r(a)),
        InstKind::CountTrailingZeros(a)  => InstKind::CountTrailingZeros(r(a)),
        InstKind::PopCount(a)            => InstKind::PopCount(r(a)),
        InstKind::IntToFloat(a, w)    => InstKind::IntToFloat(r(a), *w),
        InstKind::FloatToInt(a, w)    => InstKind::FloatToInt(r(a), *w),
        InstKind::FloatExtend(a, w)   => InstKind::FloatExtend(r(a), *w),
        InstKind::FloatTrunc(a, w)    => InstKind::FloatTrunc(r(a), *w),
        InstKind::IntExtend(a, w, s)  => InstKind::IntExtend(r(a), *w, *s),
        InstKind::IntTrunc(a, w)      => InstKind::IntTrunc(r(a), *w),
        InstKind::Alloca(t)  => InstKind::Alloca(t.clone()),
        InstKind::Load(a)    => InstKind::Load(r(a)),
        InstKind::Store(v, p) => InstKind::Store(r(v), r(p)),
        InstKind::GetElementPtr(base, idxs) =>
            InstKind::GetElementPtr(r(base), idxs.iter().map(|i| r(i)).collect()),
        InstKind::Call(f, args) =>
            InstKind::Call(f.clone(), args.iter().map(|a| r(a)).collect()),
        InstKind::RuntimeCall(f, args) =>
            InstKind::RuntimeCall(f.clone(), args.iter().map(|a| r(a)).collect()),
        InstKind::ExtractField(v, idx)     => InstKind::ExtractField(r(v), *idx),
        InstKind::InsertField(v, idx, fld) => InstKind::InsertField(r(v), *idx, r(fld)),
    }
}

/// Remap block targets and value operands in a terminator.
fn remap_terminator(
    term: &Terminator,
    block_map: &HashMap<BlockId, BlockId>,
    val_map: &HashMap<ValueId, ValueId>,
) -> Terminator {
    let rb = |b: &BlockId| *block_map.get(b).unwrap_or(b);
    let rv = |v: &ValueId| *val_map.get(v).unwrap_or(v);
    let rvs = |vs: &[ValueId]| -> Vec<ValueId> { vs.iter().map(|v| rv(v)).collect() };
    match term {
        Terminator::Return(v) => Terminator::Return(v.map(|x| rv(&x))),
        Terminator::Branch(dest, args) => Terminator::Branch(rb(dest), rvs(args)),
        Terminator::CondBranch { cond, true_dest, true_args, false_dest, false_args } =>
            Terminator::CondBranch {
                cond: rv(cond),
                true_dest: rb(true_dest),
                true_args: rvs(true_args),
                false_dest: rb(false_dest),
                false_args: rvs(false_args),
            },
        Terminator::Switch { selector, default, cases } =>
            Terminator::Switch {
                selector: rv(selector),
                default: rb(default),
                cases: cases.iter().map(|(v, d)| (*v, rb(d))).collect(),
            },
        Terminator::Unreachable => Terminator::Unreachable,
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IrType, IntWidth};
    use crate::opt::pass::Pass;
    use crate::lexer::{Span, Position};

    fn span() -> Span {
        let pos = Position { line: 0, col: 0 };
        Span { file_id: 0, start: pos, end: pos }
    }

    /// Build: entry → preheader → header(%i) → cmp → body → cond_branch(flag, t_body, f_body) →
    ///        t_body → latch; f_body → latch; latch → header
    ///
    /// `flag` is defined in entry (loop-invariant).
    fn build_unswitchable_loop() -> Module {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);

        let preheader = f.create_block("preheader");
        let header    = f.create_block("header");
        let cmp_blk   = f.create_block("cmp");
        let body      = f.create_block("body");
        let t_body    = f.create_block("t_body");
        let f_body    = f.create_block("f_body");
        let latch     = f.create_block("latch");
        let exit      = f.create_block("exit");
        let entry     = f.entry;

        // Entry: flag = const_bool(true), c1 = const 1, c10 = const 10
        let flag = f.next_value_id();
        f.register_type(flag, IrType::Bool);
        f.block_mut(entry).insts.push(Inst {
            id: flag, ty: IrType::Bool, span: span(),
            kind: InstKind::ConstBool(true),
        });
        let c1 = f.next_value_id();
        f.register_type(c1, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c1, ty: IrType::Int(IntWidth::I32), span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let c10 = f.next_value_id();
        f.register_type(c10, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c10, ty: IrType::Int(IntWidth::I32), span: span(),
            kind: InstKind::ConstInt(10, IntWidth::I32),
        });
        f.block_mut(entry).terminator = Some(Terminator::Branch(preheader, vec![]));

        // Preheader → header(c1)
        f.block_mut(preheader).terminator = Some(Terminator::Branch(header, vec![c1]));

        // Header(%i) → cmp
        let iv = f.next_value_id();
        f.register_type(iv, IrType::Int(IntWidth::I32));
        f.block_mut(header).params.push(BlockParam { id: iv, ty: IrType::Int(IntWidth::I32) });
        f.block_mut(header).terminator = Some(Terminator::Branch(cmp_blk, vec![]));

        // Cmp: icmp le %i, 10; condBr body, exit
        let cmp_v = f.next_value_id();
        f.register_type(cmp_v, IrType::Bool);
        f.block_mut(cmp_blk).insts.push(Inst {
            id: cmp_v, ty: IrType::Bool, span: span(),
            kind: InstKind::ICmp(CmpOp::Le, iv, c10),
        });
        f.block_mut(cmp_blk).terminator = Some(Terminator::CondBranch {
            cond: cmp_v,
            true_dest: body, true_args: vec![],
            false_dest: exit, false_args: vec![],
        });

        // Body: condBr flag, t_body, f_body  ← the unswitchable conditional
        f.block_mut(body).terminator = Some(Terminator::CondBranch {
            cond: flag,
            true_dest: t_body, true_args: vec![],
            false_dest: f_body, false_args: vec![],
        });

        // t_body → latch
        f.block_mut(t_body).terminator = Some(Terminator::Branch(latch, vec![]));

        // f_body → latch
        f.block_mut(f_body).terminator = Some(Terminator::Branch(latch, vec![]));

        // Latch: iadd + br header
        let nxt = f.next_value_id();
        f.register_type(nxt, IrType::Int(IntWidth::I32));
        f.block_mut(latch).insts.push(Inst {
            id: nxt, ty: IrType::Int(IntWidth::I32), span: span(),
            kind: InstKind::IAdd(iv, c1),
        });
        f.block_mut(latch).terminator = Some(Terminator::Branch(header, vec![nxt]));

        // Exit
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        m.add_function(f);
        m
    }

    #[test]
    fn unswitches_invariant_conditional() {
        let mut m = build_unswitchable_loop();
        let pass = LoopUnswitch;
        let changed = pass.run(&mut m);
        assert!(changed, "should unswitch the invariant conditional");

        // After unswitching, the preheader should have a CondBranch (not a Branch).
        let f = &m.functions[0];
        let preheader = f.blocks.iter().find(|b| b.name.contains("preheader")).unwrap();
        assert!(
            matches!(&preheader.terminator, Some(Terminator::CondBranch { .. })),
            "preheader should now have a CondBranch: {:?}", preheader.terminator
        );
    }

    #[test]
    fn does_not_unswitch_variant_conditional() {
        // Build a loop where the condition IS loop-defined (the IV).
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);

        let header = f.create_block("header");
        let cmp_blk = f.create_block("cmp");
        let body = f.create_block("body");
        let latch = f.create_block("latch");
        let exit = f.create_block("exit");
        let entry = f.entry;

        let c1 = f.next_value_id();
        f.register_type(c1, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c1, ty: IrType::Int(IntWidth::I32), span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let c10 = f.next_value_id();
        f.register_type(c10, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c10, ty: IrType::Int(IntWidth::I32), span: span(),
            kind: InstKind::ConstInt(10, IntWidth::I32),
        });
        f.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![c1]));

        let iv = f.next_value_id();
        f.register_type(iv, IrType::Int(IntWidth::I32));
        f.block_mut(header).params.push(BlockParam { id: iv, ty: IrType::Int(IntWidth::I32) });
        let cmp_v = f.next_value_id();
        f.register_type(cmp_v, IrType::Bool);
        f.block_mut(header).insts.push(Inst {
            id: cmp_v, ty: IrType::Bool, span: span(),
            kind: InstKind::ICmp(CmpOp::Le, iv, c10),
        });
        f.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond: cmp_v,
            true_dest: body, true_args: vec![],
            false_dest: exit, false_args: vec![],
        });

        // Body: the "conditional" uses the IV (loop-variant).
        let iv_cmp = f.next_value_id();
        f.register_type(iv_cmp, IrType::Bool);
        f.block_mut(body).insts.push(Inst {
            id: iv_cmp, ty: IrType::Bool, span: span(),
            kind: InstKind::ICmp(CmpOp::Le, iv, c1),
        });
        f.block_mut(body).terminator = Some(Terminator::CondBranch {
            cond: iv_cmp,
            true_dest: latch, true_args: vec![],
            false_dest: latch, false_args: vec![],
        });

        let nxt = f.next_value_id();
        f.register_type(nxt, IrType::Int(IntWidth::I32));
        f.block_mut(latch).insts.push(Inst {
            id: nxt, ty: IrType::Int(IntWidth::I32), span: span(),
            kind: InstKind::IAdd(iv, c1),
        });
        f.block_mut(latch).terminator = Some(Terminator::Branch(header, vec![nxt]));
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        m.add_function(f);

        let pass = LoopUnswitch;
        let changed = pass.run(&mut m);
        assert!(!changed, "should not unswitch a loop-variant conditional");
    }
}
