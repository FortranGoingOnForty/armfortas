//! Cross-block load-store forwarding.
//!
//! Extends local LSF across basic block boundaries. For each load,
//! walks up the dominator tree looking for a store to the same address
//! with no intervening aliasing store. Uses the alias analysis oracle
//! to disambiguate pointers.
//!
//! ```text
//! block A (dominates B):
//!     store %val → %ptr
//! block B:
//!     %r = load %ptr        → %r = %val (forwarded)
//! ```

use super::alias::{self, AliasResult};
use super::pass::Pass;
use crate::ir::inst::*;
use crate::ir::types::IrType;
use crate::ir::walk::{compute_immediate_dominators, predecessors, terminator_targets};
use std::collections::{HashMap, HashSet, VecDeque};

pub struct GlobalLsf;

impl Pass for GlobalLsf {
    fn name(&self) -> &'static str {
        "global-lsf"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if global_lsf_function(func, module.layout) {
                changed = true;
            }
        }
        changed
    }
}

fn global_lsf_function(func: &mut Function, layout: crate::target::TargetLayout) -> bool {
    let idoms = compute_immediate_dominators(func);
    let preds = predecessors(func);

    let mut replacements: HashMap<ValueId, ValueId> = HashMap::new();

    for block in &func.blocks {
        for inst in &block.insts {
            if let InstKind::Load(ptr) = &inst.kind {
                if let Some(forwarded_val) =
                    find_reaching_store(layout, func, &idoms, &preds, block.id, inst.id, *ptr)
                {
                    if func
                        .value_type(forwarded_val)
                        .is_some_and(|ty| ty == inst.ty)
                    {
                        replacements.insert(inst.id, forwarded_val);
                    }
                }
            }
        }
    }

    if replacements.is_empty() {
        return false;
    }

    // Apply replacements.
    for block in &mut func.blocks {
        for inst in &mut block.insts {
            inst.kind = super::loop_utils::remap_inst_kind(&inst.kind, &replacements);
        }
        if let Some(ref mut term) = block.terminator {
            let new_term =
                super::loop_utils::remap_terminator(term, &HashMap::new(), &replacements);
            *term = new_term;
        }
    }

    true
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MemoryState {
    Forward(ValueId),
    Clean,
    Clobbered,
}

/// Find the nearest store that safely reaches the given load.
fn find_reaching_store(
    layout: crate::target::TargetLayout,
    func: &Function,
    idoms: &HashMap<BlockId, BlockId>,
    preds: &HashMap<BlockId, Vec<BlockId>>,
    load_block: BlockId,
    load_inst: ValueId,
    load_ptr: ValueId,
) -> Option<ValueId> {
    match scan_block_before_load(layout, func, load_block, load_inst, load_ptr) {
        MemoryState::Forward(val) => return Some(val),
        MemoryState::Clobbered => return None,
        MemoryState::Clean => {}
    }

    let mut current = load_block;
    while let Some(&idom) = idoms.get(&current) {
        if idom == current {
            break;
        } // entry
        current = idom;

        if !paths_to_load_are_clean(layout, func, preds, current, load_block, load_ptr) {
            return None;
        }

        match scan_block(layout, func, current, load_ptr) {
            MemoryState::Forward(val) => return Some(val),
            MemoryState::Clobbered => return None,
            MemoryState::Clean => {}
        }
    }

    None
}

fn scan_block_before_load(
    layout: crate::target::TargetLayout,
    func: &Function,
    block_id: BlockId,
    load_inst: ValueId,
    load_ptr: ValueId,
) -> MemoryState {
    let block = func.block(block_id);
    let mut last_store = None;
    let mut clobbered = false;

    for inst in &block.insts {
        if inst.id == load_inst {
            break;
        }
        update_memory_state(
            func,
            layout,
            &inst.kind,
            load_ptr,
            &mut last_store,
            &mut clobbered,
        );
    }

    if let Some(val) = last_store {
        MemoryState::Forward(val)
    } else if clobbered {
        MemoryState::Clobbered
    } else {
        MemoryState::Clean
    }
}

fn scan_block(
    layout: crate::target::TargetLayout,
    func: &Function,
    block_id: BlockId,
    load_ptr: ValueId,
) -> MemoryState {
    let block = func.block(block_id);
    let mut last_store = None;
    let mut clobbered = false;

    for inst in &block.insts {
        update_memory_state(
            func,
            layout,
            &inst.kind,
            load_ptr,
            &mut last_store,
            &mut clobbered,
        );
    }

    if let Some(val) = last_store {
        MemoryState::Forward(val)
    } else if clobbered {
        MemoryState::Clobbered
    } else {
        MemoryState::Clean
    }
}

fn update_memory_state(
    func: &Function,
    layout: crate::target::TargetLayout,
    kind: &InstKind,
    load_ptr: ValueId,
    last_store: &mut Option<ValueId>,
    clobbered: &mut bool,
) {
    match kind {
        InstKind::Store(val, ptr) => match alias::query(func, *ptr, load_ptr, layout) {
            AliasResult::MustAlias => {
                *last_store = Some(*val);
                *clobbered = false;
            }
            AliasResult::MayAlias => {
                *last_store = None;
                *clobbered = true;
            }
            AliasResult::NoAlias => {}
        },
        InstKind::Call(_, args) | InstKind::RuntimeCall(_, args) => {
            // Call boundaries use the coarser may_reach_through_call_arg
            // predicate: a callee that gets a pointer can walk to any
            // offset within the same allocation, so a precise
            // "different GEP offset → NoAlias" answer is unsound here.
            // Same fix as LocalLsf (commit e4016f6).
            let pointer_args: Vec<ValueId> = args
                .iter()
                .copied()
                .filter(|arg| value_is_pointer(func, *arg))
                .collect();
            if pointer_args
                .iter()
                .any(|arg| alias::may_reach_through_call_arg(func, load_ptr, *arg, layout))
            {
                *last_store = None;
                *clobbered = true;
            }
        }
        _ => {}
    }
}

fn value_is_pointer(func: &Function, value: ValueId) -> bool {
    if matches!(func.value_type(value), Some(IrType::Ptr(_))) {
        return true;
    }
    if func
        .params
        .iter()
        .any(|param| param.id == value && matches!(param.ty, IrType::Ptr(_)))
    {
        return true;
    }
    func.blocks
        .iter()
        .flat_map(|block| block.insts.iter())
        .find(|inst| inst.id == value)
        .map(|inst| matches!(inst.ty, IrType::Ptr(_)))
        .unwrap_or(false)
}

fn paths_to_load_are_clean(
    layout: crate::target::TargetLayout,
    func: &Function,
    preds: &HashMap<BlockId, Vec<BlockId>>,
    start_block: BlockId,
    load_block: BlockId,
    load_ptr: ValueId,
) -> bool {
    if load_block_is_revisitable_without_store(func, start_block, load_block) {
        return false;
    }

    let can_reach_load = reverse_reachable_blocks(preds, load_block);
    let mut queue = VecDeque::new();
    let mut visited = HashSet::new();

    if let Some(term) = &func.block(start_block).terminator {
        for succ in terminator_targets(term) {
            if succ != start_block && can_reach_load.contains(&succ) {
                queue.push_back(succ);
            }
        }
    }

    while let Some(block_id) = queue.pop_front() {
        if block_id == start_block {
            return false;
        }
        if block_id == load_block || !visited.insert(block_id) {
            continue;
        }

        if !matches!(
            scan_block(layout, func, block_id, load_ptr),
            MemoryState::Clean
        ) {
            return false;
        }

        if let Some(term) = &func.block(block_id).terminator {
            for succ in terminator_targets(term) {
                if succ == start_block {
                    return false;
                }
                if can_reach_load.contains(&succ) {
                    queue.push_back(succ);
                }
            }
        }
    }

    true
}

fn load_block_is_revisitable_without_store(
    func: &Function,
    store_block: BlockId,
    load_block: BlockId,
) -> bool {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::new();

    if let Some(term) = &func.block(load_block).terminator {
        for succ in terminator_targets(term) {
            if succ != store_block {
                queue.push_back(succ);
            }
        }
    }

    while let Some(block_id) = queue.pop_front() {
        if block_id == load_block {
            return true;
        }
        if block_id == store_block || !visited.insert(block_id) {
            continue;
        }
        if let Some(term) = &func.block(block_id).terminator {
            for succ in terminator_targets(term) {
                queue.push_back(succ);
            }
        }
    }

    false
}

fn reverse_reachable_blocks(
    preds: &HashMap<BlockId, Vec<BlockId>>,
    target: BlockId,
) -> HashSet<BlockId> {
    let mut visited = HashSet::new();
    let mut queue = VecDeque::from([target]);
    visited.insert(target);

    while let Some(block_id) = queue.pop_front() {
        if let Some(block_preds) = preds.get(&block_id) {
            for &pred in block_preds {
                if visited.insert(pred) {
                    queue.push_back(pred);
                }
            }
        }
    }

    visited
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IntWidth, IrType};
    use crate::opt::pass::Pass;

    #[test]
    fn global_lsf_no_op_on_empty() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = GlobalLsf;
        assert!(!pass.run(&mut m));
    }

    #[test]
    fn forwards_across_straight_line_dominator_edge() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Int(IntWidth::I32));
        let span = crate::lexer::Span {
            file_id: 0,
            start: crate::lexer::Position { line: 0, col: 0 },
            end: crate::lexer::Position { line: 0, col: 0 },
        };

        let load_block = f.create_block("load");

        let ptr = f.next_value_id();
        f.register_type(ptr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: ptr,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span,
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
        });
        let value = f.next_value_id();
        f.register_type(value, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: value,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(42, IntWidth::I32),
        });
        let store = f.next_value_id();
        f.register_type(store, IrType::Void);
        f.block_mut(f.entry).insts.push(Inst {
            id: store,
            ty: IrType::Void,
            span,
            kind: InstKind::Store(value, ptr),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(load_block, vec![]));

        let load = f.next_value_id();
        f.register_type(load, IrType::Int(IntWidth::I32));
        f.block_mut(load_block).insts.push(Inst {
            id: load,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::Load(ptr),
        });
        f.block_mut(load_block).terminator = Some(Terminator::Return(Some(load)));
        m.add_function(f);

        let pass = GlobalLsf;
        assert!(
            pass.run(&mut m),
            "straight-line dominated load should be forwarded"
        );
        let term = m.functions[0].block(load_block).terminator.clone().unwrap();
        assert!(
            matches!(term, Terminator::Return(Some(v)) if v == value),
            "return should use the stored value after forwarding"
        );
    }

    #[test]
    fn does_not_forward_across_clobbering_side_path() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Int(IntWidth::I32));
        let span = crate::lexer::Span {
            file_id: 0,
            start: crate::lexer::Position { line: 0, col: 0 },
            end: crate::lexer::Position { line: 0, col: 0 },
        };

        let safe = f.create_block("safe");
        let clobber = f.create_block("clobber");
        let load_block = f.create_block("load");

        let ptr = f.next_value_id();
        f.register_type(ptr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: ptr,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span,
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
        });
        let stored = f.next_value_id();
        f.register_type(stored, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: stored,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(42, IntWidth::I32),
        });
        let cond = f.next_value_id();
        f.register_type(cond, IrType::Bool);
        f.block_mut(f.entry).insts.push(Inst {
            id: cond,
            ty: IrType::Bool,
            span,
            kind: InstKind::ConstBool(true),
        });
        let store = f.next_value_id();
        f.register_type(store, IrType::Void);
        f.block_mut(f.entry).insts.push(Inst {
            id: store,
            ty: IrType::Void,
            span,
            kind: InstKind::Store(stored, ptr),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: safe,
            true_args: vec![],
            false_dest: clobber,
            false_args: vec![],
        });

        f.block_mut(safe).terminator = Some(Terminator::Branch(load_block, vec![]));

        let call = f.next_value_id();
        f.register_type(call, IrType::Void);
        f.block_mut(clobber).insts.push(Inst {
            id: call,
            ty: IrType::Void,
            span,
            kind: InstKind::Call(FuncRef::External("touch".into()), vec![ptr]),
        });
        f.block_mut(clobber).terminator = Some(Terminator::Branch(load_block, vec![]));

        let load = f.next_value_id();
        f.register_type(load, IrType::Int(IntWidth::I32));
        f.block_mut(load_block).insts.push(Inst {
            id: load,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::Load(ptr),
        });
        f.block_mut(load_block).terminator = Some(Terminator::Return(Some(load)));
        m.add_function(f);

        let pass = GlobalLsf;
        assert!(
            !pass.run(&mut m),
            "a clobber on one reaching path must block cross-block forwarding"
        );
        let term = m.functions[0].block(load_block).terminator.clone().unwrap();
        assert!(
            matches!(term, Terminator::Return(Some(v)) if v == load),
            "the load result must remain in place when forwarding is unsafe"
        );
    }

    #[test]
    fn does_not_forward_into_revisitable_loop_header() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("test".into(), vec![], IrType::Int(IntWidth::I32));
        let span = crate::lexer::Span {
            file_id: 0,
            start: crate::lexer::Position { line: 0, col: 0 },
            end: crate::lexer::Position { line: 0, col: 0 },
        };

        let body = f.create_block("body");
        let header = f.create_block("header");
        let exit = f.create_block("exit");

        let ptr = f.next_value_id();
        f.register_type(ptr, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: ptr,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span,
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
        });
        let init = f.next_value_id();
        f.register_type(init, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: init,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let store = f.next_value_id();
        f.register_type(store, IrType::Void);
        f.block_mut(f.entry).insts.push(Inst {
            id: store,
            ty: IrType::Void,
            span,
            kind: InstKind::Store(init, ptr),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(header, vec![]));

        let load = f.next_value_id();
        f.register_type(load, IrType::Int(IntWidth::I32));
        f.block_mut(header).insts.push(Inst {
            id: load,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::Load(ptr),
        });
        let cond = f.next_value_id();
        f.register_type(cond, IrType::Bool);
        f.block_mut(header).insts.push(Inst {
            id: cond,
            ty: IrType::Bool,
            span,
            kind: InstKind::ConstBool(true),
        });
        f.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: body,
            true_args: vec![],
            false_dest: exit,
            false_args: vec![],
        });

        let next = f.next_value_id();
        f.register_type(next, IrType::Int(IntWidth::I32));
        f.block_mut(body).insts.push(Inst {
            id: next,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(2, IntWidth::I32),
        });
        let body_store = f.next_value_id();
        f.register_type(body_store, IrType::Void);
        f.block_mut(body).insts.push(Inst {
            id: body_store,
            ty: IrType::Void,
            span,
            kind: InstKind::Store(next, ptr),
        });
        f.block_mut(body).terminator = Some(Terminator::Branch(header, vec![]));
        f.block_mut(exit).terminator = Some(Terminator::Return(Some(load)));
        m.add_function(f);

        let pass = GlobalLsf;
        assert!(
            !pass.run(&mut m),
            "a pre-loop store must not be forwarded into a header that can be revisited"
        );
    }

    #[test]
    fn forwards_across_noalias_calling_side_path() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new(
            "f".into(),
            vec![
                Param {
                    name: "dst".into(),
                    ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
                    id: ValueId(0),
                    fortran_noalias: true,
                },
                Param {
                    name: "tmp".into(),
                    ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
                    id: ValueId(1),
                    fortran_noalias: true,
                },
            ],
            IrType::Int(IntWidth::I32),
        );
        let span = crate::lexer::Span {
            file_id: 0,
            start: crate::lexer::Position { line: 0, col: 0 },
            end: crate::lexer::Position { line: 0, col: 0 },
        };

        let side = f.create_block("side");
        let join = f.create_block("join");
        let exit = f.create_block("exit");

        let stored = f.next_value_id();
        f.register_type(stored, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: stored,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(42, IntWidth::I32),
        });
        let cond = f.next_value_id();
        f.register_type(cond, IrType::Bool);
        f.block_mut(f.entry).insts.push(Inst {
            id: cond,
            ty: IrType::Bool,
            span,
            kind: InstKind::ConstBool(true),
        });
        let store = f.next_value_id();
        f.register_type(store, IrType::Void);
        f.block_mut(f.entry).insts.push(Inst {
            id: store,
            ty: IrType::Void,
            span,
            kind: InstKind::Store(stored, ValueId(0)),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: side,
            true_args: vec![],
            false_dest: join,
            false_args: vec![],
        });

        let call = f.next_value_id();
        f.register_type(call, IrType::Void);
        f.block_mut(side).insts.push(Inst {
            id: call,
            ty: IrType::Void,
            span,
            kind: InstKind::Call(FuncRef::External("touch".into()), vec![ValueId(1)]),
        });
        f.block_mut(side).terminator = Some(Terminator::Branch(join, vec![]));

        let load = f.next_value_id();
        f.register_type(load, IrType::Int(IntWidth::I32));
        f.block_mut(join).insts.push(Inst {
            id: load,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::Load(ValueId(0)),
        });
        f.block_mut(join).terminator = Some(Terminator::Branch(exit, vec![]));
        f.block_mut(exit).terminator = Some(Terminator::Return(Some(load)));
        m.add_function(f);

        let pass = GlobalLsf;
        assert!(
            pass.run(&mut m),
            "global LSF should forward across a side path that only calls through a noalias pointer"
        );
        let term = m.functions[0].block(exit).terminator.clone().unwrap();
        assert!(
            matches!(term, Terminator::Return(Some(v)) if v == stored),
            "return should use the forwarded stored value after the noalias call side path"
        );
    }

    #[test]
    fn does_not_forward_across_call_arg_sharing_alloca_base() {
        // Mirror of contained_dummy_array_ref at the global-LSF
        // level: the caller stores arr(2)=20, then calls touch(arr),
        // then reads arr(2) in a successor block.  The call receives
        // `gep %alloca, [0]` — a pointer into the same allocation
        // as the `gep %alloca, [1]` the store targets.  A precise
        // offset-based alias check would say "different offsets →
        // NoAlias" and keep the pre-call store forwardable; the
        // correct answer at a call boundary is that the callee can
        // walk to any offset within the allocation, so the store
        // must be considered clobbered.
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
        let span = crate::lexer::Span {
            file_id: 0,
            start: crate::lexer::Position { line: 0, col: 0 },
            end: crate::lexer::Position { line: 0, col: 0 },
        };

        let load_block = f.create_block("load");

        let arr_ty = IrType::Array(Box::new(IrType::Int(IntWidth::I32)), 2);
        let alloca_ptr_ty = IrType::Ptr(Box::new(arr_ty.clone()));
        let alloca = f.next_value_id();
        f.register_type(alloca, alloca_ptr_ty.clone());
        f.block_mut(f.entry).insts.push(Inst {
            id: alloca,
            ty: alloca_ptr_ty,
            span,
            kind: InstKind::Alloca(arr_ty),
        });

        let c0 = f.next_value_id();
        f.register_type(c0, IrType::Int(IntWidth::I64));
        f.block_mut(f.entry).insts.push(Inst {
            id: c0,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::ConstInt(0, IntWidth::I64),
        });
        let c1 = f.next_value_id();
        f.register_type(c1, IrType::Int(IntWidth::I64));
        f.block_mut(f.entry).insts.push(Inst {
            id: c1,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::ConstInt(1, IntWidth::I64),
        });
        let g0 = f.next_value_id();
        f.register_type(g0, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: g0,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span,
            kind: InstKind::GetElementPtr(alloca, vec![c0]),
        });
        let g1 = f.next_value_id();
        f.register_type(g1, IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
        f.block_mut(f.entry).insts.push(Inst {
            id: g1,
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span,
            kind: InstKind::GetElementPtr(alloca, vec![c1]),
        });

        let v20 = f.next_value_id();
        f.register_type(v20, IrType::Int(IntWidth::I32));
        f.block_mut(f.entry).insts.push(Inst {
            id: v20,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::ConstInt(20, IntWidth::I32),
        });
        let store = f.next_value_id();
        f.register_type(store, IrType::Void);
        f.block_mut(f.entry).insts.push(Inst {
            id: store,
            ty: IrType::Void,
            span,
            kind: InstKind::Store(v20, g1),
        });
        let call = f.next_value_id();
        f.register_type(call, IrType::Void);
        f.block_mut(f.entry).insts.push(Inst {
            id: call,
            ty: IrType::Void,
            span,
            kind: InstKind::Call(FuncRef::External("touch".into()), vec![g0]),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(load_block, vec![]));

        let load = f.next_value_id();
        f.register_type(load, IrType::Int(IntWidth::I32));
        f.block_mut(load_block).insts.push(Inst {
            id: load,
            ty: IrType::Int(IntWidth::I32),
            span,
            kind: InstKind::Load(g1),
        });
        f.block_mut(load_block).terminator = Some(Terminator::Return(Some(load)));
        m.add_function(f);

        GlobalLsf.run(&mut m);
        let term = m.functions[0].block(load_block).terminator.clone().unwrap();
        // The load must either remain or be replaced with something
        // OTHER than v20 — forwarding the pre-call constant across
        // a call that received a pointer into the same allocation
        // is the exact bug this test guards against.
        if let Terminator::Return(Some(v)) = term {
            assert_ne!(
                v, v20,
                "global LSF forwarded the pre-call store to v20 across call(g0), but g0 and g1 share the same alloca base — the callee can walk to g1 from g0"
            );
        } else {
            panic!("unexpected terminator after GlobalLsf: {:?}", term);
        }
    }

    #[test]
    fn does_not_forward_scalar_store_into_aggregate_load() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let span = crate::lexer::Span {
            file_id: 0,
            start: crate::lexer::Position { line: 0, col: 0 },
            end: crate::lexer::Position { line: 0, col: 0 },
        };

        let load_block = f.create_block("load");
        let arr_ty = IrType::Array(
            Box::new(IrType::Float(crate::ir::types::FloatWidth::F64)),
            2,
        );

        let alloca = f.next_value_id();
        f.register_type(alloca, IrType::Ptr(Box::new(arr_ty.clone())));
        f.block_mut(f.entry).insts.push(Inst {
            id: alloca,
            ty: IrType::Ptr(Box::new(arr_ty.clone())),
            span,
            kind: InstKind::Alloca(arr_ty.clone()),
        });

        let zero = f.next_value_id();
        f.register_type(zero, IrType::Int(IntWidth::I64));
        f.block_mut(f.entry).insts.push(Inst {
            id: zero,
            ty: IrType::Int(IntWidth::I64),
            span,
            kind: InstKind::ConstInt(0, IntWidth::I64),
        });

        let elem_ptr = f.next_value_id();
        f.register_type(
            elem_ptr,
            IrType::Ptr(Box::new(IrType::Float(crate::ir::types::FloatWidth::F64))),
        );
        f.block_mut(f.entry).insts.push(Inst {
            id: elem_ptr,
            ty: IrType::Ptr(Box::new(IrType::Float(crate::ir::types::FloatWidth::F64))),
            span,
            kind: InstKind::GetElementPtr(alloca, vec![zero]),
        });

        let scalar = f.next_value_id();
        f.register_type(scalar, IrType::Float(crate::ir::types::FloatWidth::F64));
        f.block_mut(f.entry).insts.push(Inst {
            id: scalar,
            ty: IrType::Float(crate::ir::types::FloatWidth::F64),
            span,
            kind: InstKind::ConstFloat(1.5, crate::ir::types::FloatWidth::F64),
        });

        let store = f.next_value_id();
        f.register_type(store, IrType::Void);
        f.block_mut(f.entry).insts.push(Inst {
            id: store,
            ty: IrType::Void,
            span,
            kind: InstKind::Store(scalar, elem_ptr),
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(load_block, vec![]));

        let load = f.next_value_id();
        f.register_type(load, arr_ty.clone());
        f.block_mut(load_block).insts.push(Inst {
            id: load,
            ty: arr_ty,
            span,
            kind: InstKind::Load(alloca),
        });
        f.block_mut(load_block).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        let pass = GlobalLsf;
        assert!(
            !pass.run(&mut m),
            "global-lsf must not forward a scalar store into an aggregate load"
        );

        let func = &m.functions[0];
        let load_inst = func
            .block(load_block)
            .insts
            .iter()
            .find(|inst| inst.id == load)
            .expect("load should still exist");
        assert!(
            matches!(load_inst.kind, InstKind::Load(ptr) if ptr == alloca),
            "aggregate load should remain untouched, got {:?}",
            load_inst.kind
        );
    }
}
