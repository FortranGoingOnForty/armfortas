//! Bounds Check Elimination (BCE).
//!
//! Removes `RuntimeCall(CheckBounds, [index, lower, upper])` calls
//! when the index can be proven in-bounds. At O2+, this eliminates
//! the overhead of runtime bounds checking for safe array accesses.
//!
//! Safe patterns:
//! - Loop IV in a counted loop with bounds [lo, hi] where lo >= lower
//!   and hi <= upper
//! - Constant index within [lower, upper]
//!
//! Bounds checks are inserted during lowering for scalar array
//! element accesses. This pass removes the checks when the index is
//! provably safe.

use crate::ir::inst::*;
use crate::ir::walk::{find_natural_loops, predecessors};
use super::loop_utils::{find_preheader, resolve_const_int};
use super::pass::Pass;

pub struct Bce;

impl Pass for Bce {
    fn name(&self) -> &'static str { "bce" }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if bce_function(func) { changed = true; }
        }
        changed
    }
}

fn bce_function(func: &mut Function) -> bool {
    let loops = find_natural_loops(func);
    let preds = predecessors(func);
    let mut to_remove: Vec<(BlockId, usize)> = Vec::new();

    for block in &func.blocks {
        for (inst_idx, inst) in block.insts.iter().enumerate() {
            if let InstKind::RuntimeCall(RuntimeFunc::CheckBounds, args) = &inst.kind {
                if args.len() == 3 {
                    let index = args[0];
                    let lower = args[1];
                    let upper = args[2];

                    if is_provably_safe(func, &loops, &preds, index, lower, upper) {
                        to_remove.push((block.id, inst_idx));
                    }
                }
            }
        }
    }

    if to_remove.is_empty() { return false; }

    // Remove in reverse order to preserve indices.
    to_remove.sort_by(|a, b| b.1.cmp(&a.1));
    for (block_id, inst_idx) in to_remove {
        func.block_mut(block_id).insts.remove(inst_idx);
    }

    true
}

/// Check if an array index is provably within [lower, upper].
fn is_provably_safe(
    func: &Function,
    loops: &[crate::ir::walk::NaturalLoop],
    preds: &std::collections::HashMap<BlockId, Vec<BlockId>>,
    index: ValueId,
    lower: ValueId,
    upper: ValueId,
) -> bool {
    let index = strip_int_casts(func, index);

    // Case 1: constant index, constant bounds.
    if let (Some(idx), Some(lo), Some(hi)) = (
        resolve_int_scalar(func, index),
        resolve_int_scalar(func, lower),
        resolve_int_scalar(func, upper),
    ) {
        return idx >= lo && idx <= hi;
    }

    // Case 2: index is a canonical loop IV, and the loop's closed range
    // stays within the checked bounds.
    let Some(lo_const) = resolve_int_scalar(func, lower) else { return false; };
    let Some(hi_const) = resolve_int_scalar(func, upper) else { return false; };
    for lp in loops {
        let Some((range_lo, range_hi)) = loop_index_range(func, lp, preds, index) else {
            continue;
        };
        if range_lo >= lo_const && range_hi <= hi_const {
            return true;
        }
    }

    false
}

fn loop_index_range(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    preds: &std::collections::HashMap<BlockId, Vec<BlockId>>,
    index: ValueId,
) -> Option<(i64, i64)> {
    let header = func.block(lp.header);
    let param_idx = header.params.iter().position(|param| param.id == index)?;
    let init = loop_init_const(func, lp, preds, param_idx)?;
    let (bound, dir) = loop_bound_const(func, lp.header, index)?;
    let step = loop_step_const(func, lp, param_idx, index)?;

    match (dir, step.signum()) {
        (LoopDir::Ascending, 1) | (LoopDir::Descending, -1) => {
            Some((init.min(bound), init.max(bound)))
        }
        _ => None,
    }
}

fn loop_init_const(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    preds: &std::collections::HashMap<BlockId, Vec<BlockId>>,
    param_idx: usize,
) -> Option<i64> {
    let preheader = find_preheader(func, lp, preds)?;
    match &func.block(preheader).terminator {
        Some(Terminator::Branch(dest, args))
            if *dest == lp.header && param_idx < args.len() =>
        {
            resolve_int_scalar(func, args[param_idx])
        }
        _ => None,
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
enum LoopDir {
    Ascending,
    Descending,
}

fn loop_bound_const(
    func: &Function,
    header: BlockId,
    index: ValueId,
) -> Option<(i64, LoopDir)> {
    for inst in &func.block(header).insts {
        let InstKind::ICmp(op, lhs, rhs) = &inst.kind else { continue };
        match op {
            CmpOp::Le => {
                if *lhs == index {
                    return resolve_int_scalar(func, *rhs)
                        .map(|bound| (bound, LoopDir::Ascending));
                }
                if *rhs == index {
                    return resolve_int_scalar(func, *lhs)
                        .map(|bound| (bound, LoopDir::Descending));
                }
            }
            CmpOp::Ge => {
                if *lhs == index {
                    return resolve_int_scalar(func, *rhs)
                        .map(|bound| (bound, LoopDir::Descending));
                }
                if *rhs == index {
                    return resolve_int_scalar(func, *lhs)
                        .map(|bound| (bound, LoopDir::Ascending));
                }
            }
            _ => {}
        }
    }
    None
}

fn loop_step_const(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    param_idx: usize,
    index: ValueId,
) -> Option<i64> {
    let mut step: Option<i64> = None;
    for &latch in &lp.latches {
        let next =
            edge_arg_value(func.block(latch).terminator.as_ref()?, lp.header, param_idx)?;
        let latch_step = update_step_const(func, index, next)?;
        if latch_step == 0 {
            return None;
        }
        match step {
            Some(prev) if prev != latch_step => return None,
            Some(_) => {}
            None => step = Some(latch_step),
        }
    }
    step
}

fn edge_arg_value(term: &Terminator, target: BlockId, param_idx: usize) -> Option<ValueId> {
    match term {
        Terminator::Branch(dest, args) if *dest == target => args.get(param_idx).copied(),
        Terminator::CondBranch {
            true_dest,
            true_args,
            false_dest,
            false_args,
            ..
        } => {
            if *true_dest == target {
                true_args.get(param_idx).copied()
            } else if *false_dest == target {
                false_args.get(param_idx).copied()
            } else {
                None
            }
        }
        _ => None,
    }
}

fn update_step_const(func: &Function, index: ValueId, next: ValueId) -> Option<i64> {
    let next = strip_int_casts(func, next);
    if next == index {
        return Some(0);
    }

    let kind = find_inst_kind(func, next)?;
    match kind {
        InstKind::IAdd(lhs, rhs) => {
            if *lhs == index {
                resolve_int_scalar(func, *rhs)
            } else if *rhs == index {
                resolve_int_scalar(func, *lhs)
            } else {
                None
            }
        }
        InstKind::ISub(lhs, rhs) if *lhs == index => {
            resolve_int_scalar(func, *rhs).map(|step| -step)
        }
        _ => None,
    }
}

fn resolve_int_scalar(func: &Function, value: ValueId) -> Option<i64> {
    let kind = find_inst_kind(func, value)?;
    match kind {
        InstKind::ConstInt(v, _) => Some(*v),
        InstKind::IntExtend(src, _, _) | InstKind::IntTrunc(src, _) => {
            resolve_int_scalar(func, *src)
        }
        _ => resolve_const_int(func, value),
    }
}

fn strip_int_casts(func: &Function, mut value: ValueId) -> ValueId {
    loop {
        let Some(kind) = find_inst_kind(func, value) else { return value };
        match kind {
            InstKind::IntExtend(src, _, _) | InstKind::IntTrunc(src, _) => value = *src,
            _ => return value,
        }
    }
}

fn find_inst_kind(func: &Function, value: ValueId) -> Option<&InstKind> {
    for block in &func.blocks {
        for inst in &block.insts {
            if inst.id == value {
                return Some(&inst.kind);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IrType, IntWidth};
    use crate::opt::pass::Pass;

    #[test]
    fn bce_no_op_without_bounds_checks() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = Bce;
        assert!(!pass.run(&mut m), "no CheckBounds → no elimination");
    }

    #[test]
    fn bce_removes_constant_in_bounds() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let span = crate::lexer::Span {
            file_id: 0,
            start: crate::lexer::Position { line: 0, col: 0 },
            end: crate::lexer::Position { line: 0, col: 0 },
        };

        // index = 3, lower = 1, upper = 10 → safe
        let idx = f.next_value_id();
        f.register_type(idx, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: idx, ty: IrType::Int(IntWidth::I32), span,
            kind: InstKind::ConstInt(3, IntWidth::I32),
        });
        let lo = f.next_value_id();
        f.register_type(lo, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: lo, ty: IrType::Int(IntWidth::I32), span,
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let hi = f.next_value_id();
        f.register_type(hi, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: hi, ty: IrType::Int(IntWidth::I32), span,
            kind: InstKind::ConstInt(10, IntWidth::I32),
        });
        let check = f.next_value_id();
        f.register_type(check, IrType::Void);
        f.block_mut(f.entry).insts.push(Inst {
            id: check, ty: IrType::Void, span,
            kind: InstKind::RuntimeCall(RuntimeFunc::CheckBounds, vec![idx, lo, hi]),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let pass = Bce;
        let changed = pass.run(&mut m);
        assert!(changed, "constant 3 in [1,10] should be eliminated");

        // Verify the CheckBounds call was removed.
        let has_check = m.functions[0].blocks[0].insts.iter()
            .any(|i| matches!(i.kind, InstKind::RuntimeCall(RuntimeFunc::CheckBounds, _)));
        assert!(!has_check, "CheckBounds should be removed");
    }

    #[test]
    fn bce_removes_canonical_loop_iv_check() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let span = crate::lexer::Span {
            file_id: 0,
            start: crate::lexer::Position { line: 0, col: 0 },
            end: crate::lexer::Position { line: 0, col: 0 },
        };

        let header = f.create_block("header");
        let body = f.create_block("body");
        let exit = f.create_block("exit");

        let init = f.next_value_id();
        f.register_type(init, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: init,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let zero = f.next_value_id();
        f.register_type(zero, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: zero,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(0, IntWidth::I32),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(header, vec![init, zero]));

        let iv = f.next_value_id();
        f.register_type(iv, IrType::Int(IntWidth::I32));
        let sum = f.next_value_id();
        f.register_type(sum, IrType::Int(IntWidth::I32));
        f.block_mut(header).params.push(BlockParam { id: iv, ty: IrType::Int(IntWidth::I32) });
        f.block_mut(header).params.push(BlockParam { id: sum, ty: IrType::Int(IntWidth::I32) });

        let bound = f.next_value_id();
        f.register_type(bound, IrType::Int(IntWidth::I32));
        f.block_mut(header).insts.push(Inst {
            id: bound,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(4, IntWidth::I32),
        });
        let cond = f.next_value_id();
        f.register_type(cond, IrType::Bool);
        f.block_mut(header).insts.push(Inst {
            id: cond,
            ty: IrType::Bool,
            span,
            kind: InstKind::ICmp(CmpOp::Le, iv, bound),
        });
        f.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: body,
            true_args: vec![],
            false_dest: exit,
            false_args: vec![],
        });

        let iv64 = f.next_value_id();
        f.register_type(iv64, IrType::Int(IntWidth::I64));
        f.block_mut(body).insts.push(Inst {
            id: iv64,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::IntExtend(iv, IntWidth::I64, true),
        });
        let lo = f.next_value_id();
        f.register_type(lo, IrType::Int(IntWidth::I64));
        f.block_mut(body).insts.push(Inst {
            id: lo,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::ConstInt(1, IntWidth::I64),
        });
        let hi = f.next_value_id();
        f.register_type(hi, IrType::Int(IntWidth::I64));
        f.block_mut(body).insts.push(Inst {
            id: hi,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::ConstInt(4, IntWidth::I64),
        });
        let check = f.next_value_id();
        f.register_type(check, IrType::Void);
        f.block_mut(body).insts.push(Inst {
            id: check,
            ty: IrType::Void,
            span,
            kind: InstKind::RuntimeCall(RuntimeFunc::CheckBounds, vec![iv64, lo, hi]),
        });
        let step = f.next_value_id();
        f.register_type(step, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: step,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let next_iv = f.next_value_id();
        f.register_type(next_iv, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: next_iv,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::IAdd(iv, step),
        });
        let next_sum = f.next_value_id();
        f.register_type(next_sum, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: next_sum,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::IAdd(sum, step),
        });
        f.block_mut(body).terminator =
            Some(Terminator::Branch(header, vec![next_iv, next_sum]));
        f.block_mut(exit).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let pass = Bce;
        assert!(pass.run(&mut m), "canonical counted-loop bounds check should be removed");
        let has_check = m.functions[0].block(body).insts.iter().any(|inst| {
            matches!(inst.kind, InstKind::RuntimeCall(RuntimeFunc::CheckBounds, _))
        });
        assert!(!has_check, "loop body should no longer contain CheckBounds");
    }

    #[test]
    fn bce_keeps_loop_check_when_bounds_are_tighter_than_trip_range() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let span = crate::lexer::Span {
            file_id: 0,
            start: crate::lexer::Position { line: 0, col: 0 },
            end: crate::lexer::Position { line: 0, col: 0 },
        };

        let header = f.create_block("header");
        let body = f.create_block("body");
        let exit = f.create_block("exit");

        let init = f.next_value_id();
        f.register_type(init, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: init,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let zero = f.next_value_id();
        f.register_type(zero, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: zero,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(0, IntWidth::I32),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(header, vec![init, zero]));

        let iv = f.next_value_id();
        f.register_type(iv, IrType::Int(IntWidth::I32));
        let sum = f.next_value_id();
        f.register_type(sum, IrType::Int(IntWidth::I32));
        f.block_mut(header).params.push(BlockParam { id: iv, ty: IrType::Int(IntWidth::I32) });
        f.block_mut(header).params.push(BlockParam { id: sum, ty: IrType::Int(IntWidth::I32) });

        let bound = f.next_value_id();
        f.register_type(bound, IrType::Int(IntWidth::I32));
        f.block_mut(header).insts.push(Inst {
            id: bound,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(4, IntWidth::I32),
        });
        let cond = f.next_value_id();
        f.register_type(cond, IrType::Bool);
        f.block_mut(header).insts.push(Inst {
            id: cond,
            ty: IrType::Bool,
            span,
            kind: InstKind::ICmp(CmpOp::Le, iv, bound),
        });
        f.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: body,
            true_args: vec![],
            false_dest: exit,
            false_args: vec![],
        });

        let iv64 = f.next_value_id();
        f.register_type(iv64, IrType::Int(IntWidth::I64));
        f.block_mut(body).insts.push(Inst {
            id: iv64,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::IntExtend(iv, IntWidth::I64, true),
        });
        let lo = f.next_value_id();
        f.register_type(lo, IrType::Int(IntWidth::I64));
        f.block_mut(body).insts.push(Inst {
            id: lo,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::ConstInt(1, IntWidth::I64),
        });
        let hi = f.next_value_id();
        f.register_type(hi, IrType::Int(IntWidth::I64));
        f.block_mut(body).insts.push(Inst {
            id: hi,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::ConstInt(3, IntWidth::I64),
        });
        let check = f.next_value_id();
        f.register_type(check, IrType::Void);
        f.block_mut(body).insts.push(Inst {
            id: check,
            ty: IrType::Void,
            span,
            kind: InstKind::RuntimeCall(RuntimeFunc::CheckBounds, vec![iv64, lo, hi]),
        });
        let step = f.next_value_id();
        f.register_type(step, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: step,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let next_iv = f.next_value_id();
        f.register_type(next_iv, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: next_iv,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::IAdd(iv, step),
        });
        let next_sum = f.next_value_id();
        f.register_type(next_sum, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: next_sum,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::IAdd(sum, step),
        });
        f.block_mut(body).terminator =
            Some(Terminator::Branch(header, vec![next_iv, next_sum]));
        f.block_mut(exit).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let pass = Bce;
        assert!(
            !pass.run(&mut m),
            "loop trip range 1..4 exceeds checked upper bound 3, so CheckBounds must remain"
        );
    }
}
