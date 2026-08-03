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

use super::alias::{AliasOracle, AliasResult};
use super::loop_utils::{clone_loop, find_preheader, loop_defined_values};
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
        let layout = module.layout;
        for func in &mut module.functions {
            if fission_in_function(func, layout) {
                changed = true;
            }
        }
        changed
    }
}

fn fission_in_function(func: &mut Function, layout: crate::target::TargetLayout) -> bool {
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

        // Calls and vector stores would be cloned into both loops, so this
        // store-only partitioner cannot preserve their effects.
        if lp.body.iter().any(|&bid| {
            func.block(bid).insts.iter().any(|inst| {
                matches!(
                    inst.kind,
                    InstKind::Call(..)
                        | InstKind::RuntimeCall(..)
                        | InstKind::VStore(..)
                        | InstKind::VolatileLoad(..)
                        | InstKind::VolatileStore(..)
                )
            })
        }) {
            continue;
        }

        let loop_defs = loop_defined_values(func, lp);
        let slice_a = backward_slice(func, stores[0].1, &loop_defs);
        let slice_b = backward_slice(func, stores[1].1, &loop_defs);
        if !partitions_are_memory_independent(func, &slice_a, &slice_b, layout) {
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

/// Fission changes `A(i); B(i)` into all A iterations followed by all B
/// iterations. That schedule is legal only when every cross-partition pair
/// involving a write is proven disjoint. Distinct SSA base values are not a
/// proof: the opposite partition may read an array written by this one, and
/// Fortran pointers may expose the same target through separate descriptors.
fn partitions_are_memory_independent(
    func: &Function,
    slice_a: &HashSet<ValueId>,
    slice_b: &HashSet<ValueId>,
    layout: crate::target::TargetLayout,
) -> bool {
    let accesses_a = partition_memory_accesses(func, slice_a);
    let accesses_b = partition_memory_accesses(func, slice_b);
    let mut alias_oracle = AliasOracle::new(func, layout);

    for &(ptr_a, write_a) in &accesses_a {
        for &(ptr_b, write_b) in &accesses_b {
            if !write_a && !write_b {
                continue;
            }
            if !matches!(alias_oracle.query(ptr_a, ptr_b), AliasResult::NoAlias) {
                return false;
            }
        }
    }
    true
}

fn partition_memory_accesses(func: &Function, slice: &HashSet<ValueId>) -> Vec<(ValueId, bool)> {
    let mut accesses = Vec::new();
    for block in &func.blocks {
        for inst in &block.insts {
            if !slice.contains(&inst.id) {
                continue;
            }
            match inst.kind {
                InstKind::Load(ptr) | InstKind::VolatileLoad(ptr) | InstKind::VLoad(ptr) => {
                    accesses.push((ptr, false))
                }
                InstKind::Store(_, ptr)
                | InstKind::VolatileStore(_, ptr)
                | InstKind::VStore(_, ptr) => {
                    accesses.push((ptr, true));
                }
                _ => {}
            }
        }
    }
    accesses
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

    fn push_inst(func: &mut Function, kind: InstKind, ty: IrType) -> ValueId {
        let id = func.next_value_id();
        func.register_type(id, ty.clone());
        func.block_mut(func.entry).insts.push(Inst {
            id,
            ty,
            span: span(),
            kind,
        });
        id
    }

    #[test]
    fn fission_no_op_on_empty() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = LoopFission;
        let changed = pass.run(&mut m);
        assert!(!changed, "no loops → no fission");
    }

    #[test]
    fn fission_requires_cross_partition_memory_independence() {
        let ptr_ty = IrType::Ptr(Box::new(IrType::Int(IntWidth::I32)));
        let params = ["a", "b", "c", "d"]
            .into_iter()
            .enumerate()
            .map(|(id, name)| Param {
                name: name.into(),
                ty: ptr_ty.clone(),
                id: ValueId(id as u32),
                fortran_noalias: true,
            })
            .collect();
        let mut func = Function::new("partition".into(), params, IrType::Void);

        let read_b = push_inst(
            &mut func,
            InstKind::Load(ValueId(1)),
            IrType::Int(IntWidth::I32),
        );
        let write_a = push_inst(&mut func, InstKind::Store(read_b, ValueId(0)), IrType::Void);
        let read_a = push_inst(
            &mut func,
            InstKind::Load(ValueId(0)),
            IrType::Int(IntWidth::I32),
        );
        let write_b = push_inst(&mut func, InstKind::Store(read_a, ValueId(1)), IrType::Void);
        let read_d = push_inst(
            &mut func,
            InstKind::Load(ValueId(3)),
            IrType::Int(IntWidth::I32),
        );
        let write_c = push_inst(&mut func, InstKind::Store(read_d, ValueId(2)), IrType::Void);

        let partition_a = HashSet::from([read_b, write_a]);
        let dependent_partition = HashSet::from([read_a, write_b]);
        let independent_partition = HashSet::from([read_d, write_c]);
        assert!(
            !partitions_are_memory_independent(
                &func,
                &partition_a,
                &dependent_partition,
                crate::target::TargetLayout::LP64,
            ),
            "a cross-array producer/consumer cycle must block fission"
        );
        assert!(
            partitions_are_memory_independent(
                &func,
                &partition_a,
                &independent_partition,
                crate::target::TargetLayout::LP64,
            ),
            "disjoint statement groups should remain fission candidates"
        );
    }

    #[test]
    fn fission_clears_exit_args_when_rerouting_to_bridge() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
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
