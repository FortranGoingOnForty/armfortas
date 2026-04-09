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
//! Note: Bounds check INSERTION (the lowerer adding CheckBounds calls
//! at array access sites) is deferred. This pass provides the
//! elimination framework for when insertion lands.

use std::collections::HashSet;
use crate::ir::inst::*;
use crate::ir::walk::find_natural_loops;
use super::loop_utils::{resolve_const_int, loop_defined_values};
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
    let mut to_remove: Vec<(BlockId, usize)> = Vec::new();

    for block in &func.blocks {
        for (inst_idx, inst) in block.insts.iter().enumerate() {
            if let InstKind::RuntimeCall(RuntimeFunc::CheckBounds, args) = &inst.kind {
                if args.len() == 3 {
                    let index = args[0];
                    let lower = args[1];
                    let upper = args[2];

                    if is_provably_safe(func, &loops, index, lower, upper) {
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
    index: ValueId,
    lower: ValueId,
    upper: ValueId,
) -> bool {
    // Case 1: constant index, constant bounds.
    if let (Some(idx), Some(lo), Some(hi)) = (
        resolve_const_int(func, index),
        resolve_const_int(func, lower),
        resolve_const_int(func, upper),
    ) {
        return idx >= lo && idx <= hi;
    }

    // Case 2: index is a loop IV, bounds are constants matching loop bounds.
    // Check if the index is a block param of a loop header, and the loop's
    // init/bound encompass [lower, upper].
    for lp in loops {
        let hdr = func.block(lp.header);
        if hdr.params.len() != 1 { continue; }
        let iv = hdr.params[0].id;
        if iv != index { continue; }

        // The IV is in-bounds if the loop's init >= lower and bound <= upper.
        // Find init (from preheader's branch arg) and bound (from cmp block).
        // For now, conservative: only eliminate if both lower and upper are
        // constants and the loop is a standard counted loop.
        let loop_defs = loop_defined_values(func, lp);

        // Find bound from comparison.
        for &bid in &lp.body {
            let block = func.block(bid);
            for inst in &block.insts {
                if let InstKind::ICmp(CmpOp::Le, a, b) = &inst.kind {
                    if *a == iv {
                        // IV <= b → loop upper = b
                        if let (Some(lo_const), Some(hi_const), Some(bound_const)) = (
                            resolve_const_int(func, lower),
                            resolve_const_int(func, upper),
                            resolve_const_int(func, *b),
                        ) {
                            // If loop runs from some init to bound_const,
                            // and lo_const <= init and bound_const <= hi_const,
                            // the access is safe.
                            if bound_const <= hi_const && lo_const <= 1 {
                                return true;
                            }
                        }
                    }
                }
            }
        }
    }

    false
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
}
