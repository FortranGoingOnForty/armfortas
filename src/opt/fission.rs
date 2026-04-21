//! Loop fission (distribution) pass.
//!
//! Splits a loop with independent statement groups into two loops.
//! Uses the LLVM-inspired "clone + replace-with-undef" pattern:
//!   1. Clone the entire loop
//!   2. In the original: replace group B's stores with no-ops (Undef stores)
//!   3. In the clone: replace group A's stores with no-ops
//!   4. Wire original exit → clone's preheader
//!   5. Let DCE clean up the dead instructions in later passes
//!
//! This avoids SSA domination bugs from selective instruction removal.

use super::dep_analysis::{collect_mem_refs, test_dependence};
use super::loop_utils::{clone_loop, find_preheader};
use super::pass::Pass;
use crate::ir::inst::*;
use crate::ir::walk::{find_natural_loops, predecessors};
use std::collections::HashSet;

const FISSION_MIN_BODY: usize = 4;

pub struct LoopFission;

impl Pass for LoopFission {
    fn name(&self) -> &'static str {
        "loop-fission"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if fission_in_function(func) {
                changed = true;
            }
        }
        changed
    }
}

fn fission_in_function(func: &mut Function) -> bool {
    let loops = find_natural_loops(func);
    let preds = predecessors(func);

    for lp in &loops {
        let Some(_ph_id) = find_preheader(func, lp, &preds) else {
            continue;
        };
        if lp.latches.len() != 1 {
            continue;
        }
        let latch_id = lp.latches[0];

        let hdr = func.block(lp.header);
        if hdr.params.len() != 1 {
            continue;
        }
        let iv = hdr.params[0].id;

        // Find the single computation body block.
        let body_block = find_computation_block(func, lp, latch_id);
        let Some(body_bid) = body_block else { continue };

        let block = func.block(body_bid);
        if block.insts.len() < FISSION_MIN_BODY {
            continue;
        }

        // Need exactly 2 stores to different arrays.
        let stores: Vec<(usize, ValueId)> = block
            .insts
            .iter()
            .enumerate()
            .filter(|(_, i)| matches!(i.kind, InstKind::Store(..)))
            .map(|(idx, i)| (idx, i.id))
            .collect();
        if stores.len() != 2 {
            continue;
        }

        // Check independence via dep analysis.
        let mut ivs = HashSet::new();
        ivs.insert(iv);
        let mem_refs = collect_mem_refs(func, &lp.body, &ivs);
        let writes: Vec<_> = mem_refs.iter().filter(|r| r.is_write).collect();
        if writes.len() != 2 {
            continue;
        }
        if writes[0].base == writes[1].base {
            continue;
        }

        let mut has_cross_dep = false;
        for i in 0..mem_refs.len() {
            for j in (i + 1)..mem_refs.len() {
                if !mem_refs[i].is_write && !mem_refs[j].is_write {
                    continue;
                }
                if mem_refs[i].base == mem_refs[j].base {
                    continue;
                }
                let dep = test_dependence(&mem_refs[i], &mem_refs[j]);
                if dep.dependent {
                    has_cross_dep = true;
                    break;
                }
            }
            if has_cross_dep {
                break;
            }
        }
        if has_cross_dep {
            continue;
        }

        // Find the exit block.
        let exit_id = find_loop_exit(func, lp);
        let Some(exit_id) = exit_id else { continue };

        // Compute backward slices for each store.
        // Clone the loop.
        let (block_map, _) = clone_loop(func, lp);

        // In original body: neutralize store B (replace value with undef).
        neutralize_store(func, body_bid, stores[1].0);

        // In clone body: neutralize store A.
        let clone_body = block_map[&body_bid];
        neutralize_store(func, clone_body, stores[0].0);

        // Wire original exit → clone's header via a bridge block.
        let clone_header = block_map[&lp.header];
        let ph_id = find_preheader(func, lp, &predecessors(func)).unwrap();
        let init_val = match &func.block(ph_id).terminator {
            Some(Terminator::Branch(_, args)) if !args.is_empty() => args[0],
            _ => continue,
        };

        let bridge = func.create_block("fission_bridge");
        func.block_mut(bridge).terminator = Some(Terminator::Branch(clone_header, vec![init_val]));

        // Redirect original cmp's exit → bridge.
        for &bid in &lp.body {
            let block = func.block_mut(bid);
            if let Some(Terminator::CondBranch {
                false_dest,
                false_args,
                ..
            }) = &mut block.terminator
            {
                if *false_dest == exit_id {
                    *false_dest = bridge;
                    false_args.clear();
                    break;
                }
            }
        }

        // Clone's cmp exit already points to exit_id (wasn't remapped
        // since it's outside the body). Correct.

        return true;
    }
    false
}

/// Replace a store instruction's value operand with Undef, effectively
/// making it a dead store that DSE/DCE can clean up.
fn neutralize_store(func: &mut Function, block_id: BlockId, store_idx: usize) {
    let block = func.block_mut(block_id);
    if store_idx >= block.insts.len() {
        return;
    }
    if let InstKind::Store(_, _) = block.insts[store_idx].kind {
        // Replace with a store of undef — the store is now dead.
        // We keep the store instruction (not remove it) to preserve
        // SSA structure. DCE will clean it up later.
        let undef_id = ValueId(u32::MAX - 1); // sentinel; will be replaced below
        let _ = undef_id;
    }
    // Actually, the simplest approach: just remove the store entirely.
    // Stores have no result value used by other instructions, so removing
    // them can't break SSA. The computation leading to the store value
    // becomes dead and DCE removes it.
    block.insts.remove(store_idx);
}

fn find_computation_block(
    func: &Function,
    lp: &crate::ir::walk::NaturalLoop,
    latch_id: BlockId,
) -> Option<BlockId> {
    let mut comp = None;
    for &bid in &lp.body {
        if bid == lp.header || bid == latch_id {
            continue;
        }
        let block = func.block(bid);
        if block
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::Store(..)))
        {
            if comp.is_some() {
                return None;
            }
            comp = Some(bid);
        }
    }
    comp
}

fn backward_slice(
    func: &Function,
    root: ValueId,
    loop_defs: &HashSet<ValueId>,
) -> HashSet<ValueId> {
    let mut slice = HashSet::new();
    let mut worklist = vec![root];
    while let Some(vid) = worklist.pop() {
        if !slice.insert(vid) {
            continue;
        }
        for block in &func.blocks {
            for inst in &block.insts {
                if inst.id == vid {
                    for operand in crate::ir::walk::inst_uses(&inst.kind) {
                        if loop_defs.contains(&operand) && !slice.contains(&operand) {
                            worklist.push(operand);
                        }
                    }
                }
            }
        }
    }
    slice
}

fn find_loop_exit(func: &Function, lp: &crate::ir::walk::NaturalLoop) -> Option<BlockId> {
    for &bid in &lp.body {
        let block = func.block(bid);
        if let Some(Terminator::CondBranch { false_dest, .. }) = &block.terminator {
            if !lp.body.contains(false_dest) {
                return Some(*false_dest);
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IntWidth, IrType};
    use crate::ir::verify::verify_module;
    use crate::lexer::{Position, Span};
    use crate::opt::pass::Pass;

    fn span() -> Span {
        let pos = Position { line: 0, col: 0 };
        Span {
            file_id: 0,
            start: pos,
            end: pos,
        }
    }

    #[test]
    fn fission_no_op_on_empty() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = LoopFission;
        let changed = pass.run(&mut m);
        assert!(!changed, "no loops → no fission");
    }

    #[test]
    fn fission_clears_exit_args_when_rerouting_to_bridge() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);

        let preheader = f.create_block("preheader");
        let header = f.create_block("header");
        let body = f.create_block("body");
        let latch = f.create_block("latch");
        let exit = f.create_block("exit");
        let entry = f.entry;

        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 16);
        let arr_a = f.next_value_id();
        f.register_type(arr_a, IrType::Ptr(Box::new(arr_ty.clone())));
        f.block_mut(entry).insts.push(Inst {
            id: arr_a,
            ty: IrType::Ptr(Box::new(arr_ty.clone())),
            span: span(),
            kind: InstKind::Alloca(arr_ty.clone()),
        });
        let arr_b = f.next_value_id();
        f.register_type(arr_b, IrType::Ptr(Box::new(arr_ty.clone())));
        f.block_mut(entry).insts.push(Inst {
            id: arr_b,
            ty: IrType::Ptr(Box::new(arr_ty)),
            span: span(),
            kind: InstKind::Alloca(IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 16)),
        });
        let c0 = f.next_value_id();
        f.register_type(c0, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c0,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::ConstInt(0, IntWidth::I32),
        });
        let c1 = f.next_value_id();
        f.register_type(c1, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c1,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let c2 = f.next_value_id();
        f.register_type(c2, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c2,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::ConstInt(2, IntWidth::I32),
        });
        let c4 = f.next_value_id();
        f.register_type(c4, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c4,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::ConstInt(4, IntWidth::I32),
        });
        f.block_mut(entry).terminator = Some(Terminator::Branch(preheader, vec![]));

        f.block_mut(preheader).terminator = Some(Terminator::Branch(header, vec![c0]));

        let iv = f.next_value_id();
        f.register_type(iv, IrType::Int(IntWidth::I32));
        f.block_mut(header).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I32),
        });
        f.block_mut(header).terminator = Some(Terminator::Branch(body, vec![]));

        let gep_a = f.next_value_id();
        f.register_type(gep_a, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(body).insts.push(Inst {
            id: gep_a,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::GetElementPtr(arr_a, vec![iv]),
        });
        let store_a = f.next_value_id();
        f.register_type(store_a, IrType::Void);
        f.block_mut(body).insts.push(Inst {
            id: store_a,
            ty: IrType::Void,
            span: span(),
            kind: InstKind::Store(c1, gep_a),
        });

        let gep_b = f.next_value_id();
        f.register_type(gep_b, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(body).insts.push(Inst {
            id: gep_b,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: span(),
            kind: InstKind::GetElementPtr(arr_b, vec![iv]),
        });
        let store_b = f.next_value_id();
        f.register_type(store_b, IrType::Void);
        f.block_mut(body).insts.push(Inst {
            id: store_b,
            ty: IrType::Void,
            span: span(),
            kind: InstKind::Store(c2, gep_b),
        });
        f.block_mut(body).terminator = Some(Terminator::Branch(latch, vec![]));

        let nxt = f.next_value_id();
        f.register_type(nxt, IrType::Int(IntWidth::I32));
        f.block_mut(latch).insts.push(Inst {
            id: nxt,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::IAdd(iv, c1),
        });
        let cmp = f.next_value_id();
        f.register_type(cmp, IrType::Bool);
        f.block_mut(latch).insts.push(Inst {
            id: cmp,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ICmp(CmpOp::Le, nxt, c4),
        });
        f.block_mut(latch).terminator = Some(Terminator::CondBranch {
            cond: cmp,
            true_dest: header,
            true_args: vec![nxt],
            false_dest: exit,
            false_args: vec![nxt],
        });

        let exit_param = f.next_value_id();
        f.register_type(exit_param, IrType::Int(IntWidth::I32));
        f.block_mut(exit).params.push(BlockParam {
            id: exit_param,
            ty: IrType::Int(IntWidth::I32),
        });
        let _use_exit = f.next_value_id();
        f.register_type(_use_exit, IrType::Int(IntWidth::I32));
        f.block_mut(exit).insts.push(Inst {
            id: _use_exit,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::IAdd(exit_param, c1),
        });
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        m.add_function(f);
        assert!(verify_module(&m).is_empty(), "test setup must start valid");

        let pass = LoopFission;
        let changed = pass.run(&mut m);
        assert!(changed, "the loop should fission");
        assert!(
            verify_module(&m).is_empty(),
            "fission should keep bridge exit edges verifier-clean"
        );
    }
}
