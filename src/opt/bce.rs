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

use super::loop_utils::find_preheader;
use super::pass::Pass;
use crate::ir::inst::*;
use crate::ir::walk::{find_natural_loops, predecessors};

pub struct Bce;

impl Pass for Bce {
    fn name(&self) -> &'static str {
        "bce"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if bce_function(func) {
                changed = true;
            }
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

    if to_remove.is_empty() {
        return false;
    }

    // Remove in reverse order to preserve indices.
    to_remove.sort_by_key(|entry| std::cmp::Reverse(entry.1));
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
    let index = strip_value_preserving_int_extends(func, index);

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
    let Some(lo_const) = resolve_int_scalar(func, lower) else {
        return false;
    };
    let Some(hi_const) = resolve_int_scalar(func, upper) else {
        return false;
    };
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
    let (index, scale, offset) = loop_index_base_and_offset(func, index)?;
    let header = func.block(lp.header);
    let param_idx = header.params.iter().position(|param| param.id == index)?;
    let init = loop_init_const(func, lp, preds, param_idx)?;
    let (bound, dir) = loop_bound_const(func, lp.header, index)?;
    let step = loop_step_const(func, lp, param_idx, index)?;

    let (range_lo, range_hi) = match dir {
        LoopDir::Ascending if step > 0 => {
            if init > bound {
                return None;
            }
            let trip = bound.checked_sub(init)?;
            let last = init.checked_add((trip / step).checked_mul(step)?)?;
            (init.min(last), init.max(last))
        }
        LoopDir::Descending if step < 0 => {
            if init < bound {
                return None;
            }
            let step_mag = step.checked_neg()?;
            let trip = init.checked_sub(bound)?;
            let last = init.checked_sub((trip / step_mag).checked_mul(step_mag)?)?;
            (init.min(last), init.max(last))
        }
        _ => return None,
    };
    // Apply scale and offset: idx == scale * base + offset, with
    // `base` ranging over [range_lo, range_hi]. Negative scale flips
    // the interval, so take min/max after multiplying both endpoints.
    let lo_scaled = scale.checked_mul(range_lo)?;
    let hi_scaled = scale.checked_mul(range_hi)?;
    let adj_lo = lo_scaled.min(hi_scaled).checked_add(offset)?;
    let adj_hi = lo_scaled.max(hi_scaled).checked_add(offset)?;
    Some((adj_lo, adj_hi))
}

/// Decompose `value` into a linear function of an inner SSA value:
/// `value == scale * base + offset`. The "base" returned is whatever
/// definition is left after we've stripped IAdd / ISub / IMul of
/// constant operands; if `base` is the loop induction variable, the
/// caller can then map a known IV range into a known range for
/// `value` itself.
///
/// Default decomposition is `(value, 1, 0)` (i.e. base==value, no
/// scale, no offset). Recurses through:
///  * `iadd lhs, const` / `isub lhs, const` → bumps offset.
///  * `imul lhs, const` / `imul const, rhs` → multiplies both scale
///    and offset, capturing `(2*i) + 3 = 2*i + 3` and `2*(i+1) = 2*i + 2`.
fn loop_index_base_and_offset(func: &Function, value: ValueId) -> Option<(ValueId, i64, i64)> {
    let value = strip_value_preserving_int_extends(func, value);
    let Some(kind) = find_inst_kind(func, value) else {
        return Some((value, 1, 0));
    };

    match kind {
        InstKind::IAdd(lhs, rhs) => {
            if let Some((base, scale, offset)) = loop_index_base_and_offset(func, *lhs) {
                if let Some(delta) = resolve_int_scalar(func, *rhs) {
                    return offset.checked_add(delta).map(|next| (base, scale, next));
                }
            }
            if let Some((base, scale, offset)) = loop_index_base_and_offset(func, *rhs) {
                if let Some(delta) = resolve_int_scalar(func, *lhs) {
                    return offset.checked_add(delta).map(|next| (base, scale, next));
                }
            }
            Some((value, 1, 0))
        }
        InstKind::ISub(lhs, rhs) => {
            if let Some((base, scale, offset)) = loop_index_base_and_offset(func, *lhs) {
                if let Some(delta) = resolve_int_scalar(func, *rhs) {
                    return offset.checked_sub(delta).map(|next| (base, scale, next));
                }
            }
            Some((value, 1, 0))
        }
        InstKind::IMul(lhs, rhs) => {
            // k * (s * base + o) = (k*s) * base + k*o
            if let Some((base, scale, offset)) = loop_index_base_and_offset(func, *lhs) {
                if let Some(k) = resolve_int_scalar(func, *rhs) {
                    let new_scale = scale.checked_mul(k)?;
                    let new_offset = offset.checked_mul(k)?;
                    return Some((base, new_scale, new_offset));
                }
            }
            if let Some((base, scale, offset)) = loop_index_base_and_offset(func, *rhs) {
                if let Some(k) = resolve_int_scalar(func, *lhs) {
                    let new_scale = scale.checked_mul(k)?;
                    let new_offset = offset.checked_mul(k)?;
                    return Some((base, new_scale, new_offset));
                }
            }
            Some((value, 1, 0))
        }
        _ => Some((value, 1, 0)),
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
        Some(Terminator::Branch(dest, args)) if *dest == lp.header && param_idx < args.len() => {
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

fn loop_bound_const(func: &Function, header: BlockId, index: ValueId) -> Option<(i64, LoopDir)> {
    for inst in &func.block(header).insts {
        let InstKind::ICmp(op, lhs, rhs) = &inst.kind else {
            continue;
        };
        match op {
            CmpOp::Le => {
                if *lhs == index {
                    return resolve_int_scalar(func, *rhs).map(|bound| (bound, LoopDir::Ascending));
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
                    return resolve_int_scalar(func, *lhs).map(|bound| (bound, LoopDir::Ascending));
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
        let next = edge_arg_value(func.block(latch).terminator.as_ref()?, lp.header, param_idx)?;
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
    let next = strip_value_preserving_int_extends(func, next);
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
    i64::try_from(resolve_int_scalar_i128(func, value)?).ok()
}

fn resolve_int_scalar_i128(func: &Function, value: ValueId) -> Option<i128> {
    let kind = find_inst_kind(func, value)?;
    match kind {
        InstKind::ConstInt(v, width) => Some(normalize_signed_int(*v, *width)),
        InstKind::IntExtend(src, _, true) => resolve_int_scalar_i128(func, *src),
        InstKind::IntExtend(src, _, false) => {
            let source_width = func.value_type(*src)?.int_width()?;
            resolve_int_scalar_i128(func, *src)
                .map(|source| mask_int_to_width(source, source_width))
        }
        InstKind::IntTrunc(src, width) => {
            resolve_int_scalar_i128(func, *src).map(|source| normalize_signed_int(source, *width))
        }
        _ => None,
    }
}

fn normalize_signed_int(value: i128, width: crate::ir::types::IntWidth) -> i128 {
    let bits = width.bits();
    if bits == 128 {
        value
    } else {
        let shift = 128 - bits;
        (value << shift) >> shift
    }
}

fn mask_int_to_width(value: i128, width: crate::ir::types::IntWidth) -> i128 {
    let bits = width.bits();
    if bits == 128 {
        value
    } else {
        value & ((1_i128 << bits) - 1)
    }
}

fn strip_value_preserving_int_extends(func: &Function, mut value: ValueId) -> ValueId {
    loop {
        let Some(kind) = find_inst_kind(func, value) else {
            return value;
        };
        match kind {
            InstKind::IntExtend(src, target_width, true)
                if func
                    .value_type(*src)
                    .and_then(|ty| ty.int_width())
                    .is_some_and(|source_width| source_width.bits() < target_width.bits()) =>
            {
                value = *src;
            }
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
    use crate::ir::types::{IntWidth, IrType};
    use crate::opt::pass::Pass;

    #[test]
    fn bce_no_op_without_bounds_checks() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = Bce;
        assert!(!pass.run(&mut m), "no CheckBounds → no elimination");
    }

    #[test]
    fn bce_removes_constant_in_bounds() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
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
            id: idx,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(3, IntWidth::I32),
        });
        let lo = f.next_value_id();
        f.register_type(lo, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: lo,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let hi = f.next_value_id();
        f.register_type(hi, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: hi,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(10, IntWidth::I32),
        });
        let check = f.next_value_id();
        f.register_type(check, IrType::Void);
        f.block_mut(f.entry).insts.push(Inst {
            id: check,
            ty: IrType::Void,
            span,
            kind: InstKind::RuntimeCall(RuntimeFunc::CheckBounds, vec![idx, lo, hi]),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let pass = Bce;
        let changed = pass.run(&mut m);
        assert!(changed, "constant 3 in [1,10] should be eliminated");

        // Verify the CheckBounds call was removed.
        let has_check = m.functions[0].blocks[0]
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::RuntimeCall(RuntimeFunc::CheckBounds, _)));
        assert!(!has_check, "CheckBounds should be removed");
    }

    #[test]
    fn bce_keeps_narrowed_constant_out_of_bounds() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let span = crate::lexer::Span {
            file_id: 0,
            start: crate::lexer::Position { line: 0, col: 0 },
            end: crate::lexer::Position { line: 0, col: 0 },
        };

        let wide = f.next_value_id();
        f.register_type(wide, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: wide,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(129, IntWidth::I32),
        });
        let narrow = f.next_value_id();
        f.register_type(narrow, IrType::Int(IntWidth::I8));
        f.block_mut(f.entry).insts.push(Inst {
            id: narrow,
            ty: IrType::Int(IntWidth::I8),
            span,
            kind: InstKind::IntTrunc(wide, IntWidth::I8),
        });
        let index = f.next_value_id();
        f.register_type(index, IrType::Int(IntWidth::I64));
        f.block_mut(f.entry).insts.push(Inst {
            id: index,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::IntExtend(narrow, IntWidth::I64, true),
        });
        let lo = f.next_value_id();
        f.register_type(lo, IrType::Int(IntWidth::I64));
        f.block_mut(f.entry).insts.push(Inst {
            id: lo,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::ConstInt(1, IntWidth::I64),
        });
        let hi = f.next_value_id();
        f.register_type(hi, IrType::Int(IntWidth::I64));
        f.block_mut(f.entry).insts.push(Inst {
            id: hi,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::ConstInt(200, IntWidth::I64),
        });
        let check = f.next_value_id();
        f.register_type(check, IrType::Void);
        f.block_mut(f.entry).insts.push(Inst {
            id: check,
            ty: IrType::Void,
            span,
            kind: InstKind::RuntimeCall(RuntimeFunc::CheckBounds, vec![index, lo, hi]),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        assert_eq!(
            resolve_int_scalar(&f, narrow),
            Some(-127),
            "constant truncation must use the destination width"
        );
        m.add_function(f);

        let pass = Bce;
        assert!(
            !pass.run(&mut m),
            "narrowing 129 to i8 produces -127, so the bounds check must remain"
        );
    }

    #[test]
    fn bce_removes_canonical_loop_iv_check() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
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
        f.block_mut(header).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I32),
        });
        f.block_mut(header).params.push(BlockParam {
            id: sum,
            ty: IrType::Int(IntWidth::I32),
        });

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
        f.block_mut(body).terminator = Some(Terminator::Branch(header, vec![next_iv, next_sum]));
        f.block_mut(exit).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let pass = Bce;
        assert!(
            pass.run(&mut m),
            "canonical counted-loop bounds check should be removed"
        );
        let has_check = m.functions[0].block(body).insts.iter().any(|inst| {
            matches!(
                inst.kind,
                InstKind::RuntimeCall(RuntimeFunc::CheckBounds, _)
            )
        });
        assert!(!has_check, "loop body should no longer contain CheckBounds");
    }

    #[test]
    fn bce_keeps_loop_check_when_bounds_are_tighter_than_trip_range() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
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
        f.block_mut(header).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I32),
        });
        f.block_mut(header).params.push(BlockParam {
            id: sum,
            ty: IrType::Int(IntWidth::I32),
        });

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
        f.block_mut(body).terminator = Some(Terminator::Branch(header, vec![next_iv, next_sum]));
        f.block_mut(exit).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let pass = Bce;
        assert!(
            !pass.run(&mut m),
            "loop trip range 1..4 exceeds checked upper bound 3, so CheckBounds must remain"
        );
    }

    #[test]
    fn bce_removes_loop_iv_plus_constant_check() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
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
            kind: InstKind::ConstInt(2, IntWidth::I32),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(header, vec![init]));

        let iv = f.next_value_id();
        f.register_type(iv, IrType::Int(IntWidth::I32));
        f.block_mut(header).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I32),
        });

        let bound = f.next_value_id();
        f.register_type(bound, IrType::Int(IntWidth::I32));
        f.block_mut(header).insts.push(Inst {
            id: bound,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(7, IntWidth::I32),
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

        let one = f.next_value_id();
        f.register_type(one, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: one,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let plus = f.next_value_id();
        f.register_type(plus, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: plus,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::IAdd(iv, one),
        });
        let plus64 = f.next_value_id();
        f.register_type(plus64, IrType::Int(IntWidth::I64));
        f.block_mut(body).insts.push(Inst {
            id: plus64,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::IntExtend(plus, IntWidth::I64, true),
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
            kind: InstKind::ConstInt(8, IntWidth::I64),
        });
        let check = f.next_value_id();
        f.register_type(check, IrType::Void);
        f.block_mut(body).insts.push(Inst {
            id: check,
            ty: IrType::Void,
            span,
            kind: InstKind::RuntimeCall(RuntimeFunc::CheckBounds, vec![plus64, lo, hi]),
        });
        let next_iv = f.next_value_id();
        f.register_type(next_iv, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: next_iv,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::IAdd(iv, one),
        });
        f.block_mut(body).terminator = Some(Terminator::Branch(header, vec![next_iv]));
        f.block_mut(exit).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let pass = Bce;
        assert!(
            pass.run(&mut m),
            "iv+1 check should be removed for trip range 2..7 and checked bounds 1..8"
        );
        let has_check = m.functions[0].block(body).insts.iter().any(|inst| {
            matches!(
                inst.kind,
                InstKind::RuntimeCall(RuntimeFunc::CheckBounds, _)
            )
        });
        assert!(!has_check, "loop body should no longer contain CheckBounds");
    }

    #[test]
    fn bce_removes_loop_iv_minus_constant_check() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
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
            kind: InstKind::ConstInt(2, IntWidth::I32),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(header, vec![init]));

        let iv = f.next_value_id();
        f.register_type(iv, IrType::Int(IntWidth::I32));
        f.block_mut(header).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I32),
        });

        let bound = f.next_value_id();
        f.register_type(bound, IrType::Int(IntWidth::I32));
        f.block_mut(header).insts.push(Inst {
            id: bound,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(7, IntWidth::I32),
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

        let one = f.next_value_id();
        f.register_type(one, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: one,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let minus = f.next_value_id();
        f.register_type(minus, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: minus,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ISub(iv, one),
        });
        let minus64 = f.next_value_id();
        f.register_type(minus64, IrType::Int(IntWidth::I64));
        f.block_mut(body).insts.push(Inst {
            id: minus64,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::IntExtend(minus, IntWidth::I64, true),
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
            kind: InstKind::ConstInt(8, IntWidth::I64),
        });
        let check = f.next_value_id();
        f.register_type(check, IrType::Void);
        f.block_mut(body).insts.push(Inst {
            id: check,
            ty: IrType::Void,
            span,
            kind: InstKind::RuntimeCall(RuntimeFunc::CheckBounds, vec![minus64, lo, hi]),
        });
        let next_iv = f.next_value_id();
        f.register_type(next_iv, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: next_iv,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::IAdd(iv, one),
        });
        f.block_mut(body).terminator = Some(Terminator::Branch(header, vec![next_iv]));
        f.block_mut(exit).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let pass = Bce;
        assert!(
            pass.run(&mut m),
            "iv-1 check should be removed for trip range 2..7 and checked bounds 1..8"
        );
        let has_check = m.functions[0].block(body).insts.iter().any(|inst| {
            matches!(
                inst.kind,
                InstKind::RuntimeCall(RuntimeFunc::CheckBounds, _)
            )
        });
        assert!(!has_check, "loop body should no longer contain CheckBounds");
    }

    #[test]
    fn bce_keeps_loop_iv_plus_constant_check_when_offset_exceeds_upper_bound() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
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
            kind: InstKind::ConstInt(2, IntWidth::I32),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(header, vec![init]));

        let iv = f.next_value_id();
        f.register_type(iv, IrType::Int(IntWidth::I32));
        f.block_mut(header).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I32),
        });

        let bound = f.next_value_id();
        f.register_type(bound, IrType::Int(IntWidth::I32));
        f.block_mut(header).insts.push(Inst {
            id: bound,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(7, IntWidth::I32),
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

        let two = f.next_value_id();
        f.register_type(two, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: two,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(2, IntWidth::I32),
        });
        let plus = f.next_value_id();
        f.register_type(plus, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: plus,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::IAdd(iv, two),
        });
        let plus64 = f.next_value_id();
        f.register_type(plus64, IrType::Int(IntWidth::I64));
        f.block_mut(body).insts.push(Inst {
            id: plus64,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::IntExtend(plus, IntWidth::I64, true),
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
            kind: InstKind::ConstInt(8, IntWidth::I64),
        });
        let check = f.next_value_id();
        f.register_type(check, IrType::Void);
        f.block_mut(body).insts.push(Inst {
            id: check,
            ty: IrType::Void,
            span,
            kind: InstKind::RuntimeCall(RuntimeFunc::CheckBounds, vec![plus64, lo, hi]),
        });
        let one = f.next_value_id();
        f.register_type(one, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: one,
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
            kind: InstKind::IAdd(iv, one),
        });
        f.block_mut(body).terminator = Some(Terminator::Branch(header, vec![next_iv]));
        f.block_mut(exit).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let pass = Bce;
        assert!(
            !pass.run(&mut m),
            "iv+2 check must remain when trip range 2..7 would access 9 beyond checked upper bound 8"
        );
    }

    #[test]
    fn bce_removes_loop_iv_plus_constant_check_with_large_step() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
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
            kind: InstKind::ConstInt(5, IntWidth::I32),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(header, vec![init]));

        let iv = f.next_value_id();
        f.register_type(iv, IrType::Int(IntWidth::I32));
        f.block_mut(header).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I32),
        });

        let bound = f.next_value_id();
        f.register_type(bound, IrType::Int(IntWidth::I32));
        f.block_mut(header).insts.push(Inst {
            id: bound,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(10, IntWidth::I32),
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

        let five = f.next_value_id();
        f.register_type(five, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: five,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(5, IntWidth::I32),
        });
        let plus = f.next_value_id();
        f.register_type(plus, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: plus,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::IAdd(iv, five),
        });
        let plus64 = f.next_value_id();
        f.register_type(plus64, IrType::Int(IntWidth::I64));
        f.block_mut(body).insts.push(Inst {
            id: plus64,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::IntExtend(plus, IntWidth::I64, true),
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
            kind: InstKind::ConstInt(10, IntWidth::I64),
        });
        let check = f.next_value_id();
        f.register_type(check, IrType::Void);
        f.block_mut(body).insts.push(Inst {
            id: check,
            ty: IrType::Void,
            span,
            kind: InstKind::RuntimeCall(RuntimeFunc::CheckBounds, vec![plus64, lo, hi]),
        });
        let step = f.next_value_id();
        f.register_type(step, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: step,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(6, IntWidth::I32),
        });
        let next_iv = f.next_value_id();
        f.register_type(next_iv, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: next_iv,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::IAdd(iv, step),
        });
        f.block_mut(body).terminator = Some(Terminator::Branch(header, vec![next_iv]));
        f.block_mut(exit).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let pass = Bce;
        assert!(
            pass.run(&mut m),
            "iv+5 check should be removed when the trip sequence is only 5 and checked bounds are 1..10"
        );
        let has_check = m.functions[0].block(body).insts.iter().any(|inst| {
            matches!(
                inst.kind,
                InstKind::RuntimeCall(RuntimeFunc::CheckBounds, _)
            )
        });
        assert!(!has_check, "loop body should no longer contain CheckBounds");
    }

    #[test]
    fn loop_index_decomposes_constant_multiplied_iv() {
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        let span = crate::lexer::Span {
            file_id: 0,
            start: crate::lexer::Position { line: 0, col: 0 },
            end: crate::lexer::Position { line: 0, col: 0 },
        };

        // Synthesise: i = next_value(); 2*i; (2*i) + 3 — verify the
        // decomposition (base, scale, offset) recovers (i, 2, 3).
        let entry = f.entry;
        let i = f.next_value_id();
        f.register_type(i, IrType::Int(IntWidth::I32));
        let two = f.next_value_id();
        f.register_type(two, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: two,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(2, IntWidth::I32),
        });
        let two_i = f.next_value_id();
        f.register_type(two_i, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: two_i,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::IMul(two, i),
        });
        let three = f.next_value_id();
        f.register_type(three, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: three,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(3, IntWidth::I32),
        });
        let two_i_plus_three = f.next_value_id();
        f.register_type(two_i_plus_three, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: two_i_plus_three,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::IAdd(two_i, three),
        });

        let (base, scale, offset) =
            loop_index_base_and_offset(&f, two_i_plus_three).expect("should decompose");
        assert_eq!(base, i, "base should be the unanalysable inner value");
        assert_eq!(scale, 2, "scale captures the IMul by 2");
        assert_eq!(offset, 3, "offset captures the +3");
    }
}
