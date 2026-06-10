//! Function inlining pass.
//!
//! Replaces call sites with the callee's body, enabling downstream
//! optimizations (const prop, DSE, LICM) to fire across the former
//! call boundary.
//!
//! Algorithm:
//!   1. Build call graph, process in bottom-up order (callees first)
//!   2. For each call site passing the cost model, clone the callee's
//!      blocks into the caller with fresh ValueIds/BlockIds
//!   3. Map callee params → caller args
//!   4. Replace Return(val) → Branch(post_call_block, [val])
//!   5. Split the call-containing block: pre-call instructions +
//!      branch to cloned entry, post-call block receives return value

use super::callgraph::CallGraph;
use super::loop_utils::{remap_inst_kind, remap_terminator};
use super::pass::Pass;
use super::pipeline::OptLevel;
use crate::ir::inst::*;
use crate::ir::types::IrType;
use crate::ir::walk::{prune_unreachable, substitute_uses};
use std::collections::HashMap;

/// Maximum callee instruction count for inlining.
const INLINE_THRESHOLD_O1: usize = 20;
const INLINE_THRESHOLD_O2: usize = 100;

pub struct Inline {
    threshold: usize,
}

impl Inline {
    pub fn for_level(level: OptLevel) -> Self {
        let threshold = match level {
            OptLevel::O1 => INLINE_THRESHOLD_O1,
            OptLevel::O2 | OptLevel::Os => INLINE_THRESHOLD_O2,
            OptLevel::O3 | OptLevel::Ofast => 200,
            OptLevel::O0 => 0,
        };
        Self { threshold }
    }
}

impl Pass for Inline {
    fn name(&self) -> &'static str {
        "inline"
    }

    fn run(&self, module: &mut Module) -> bool {
        if self.threshold == 0 {
            return false;
        }

        let cg = CallGraph::build(module);
        let order = cg.bottom_up_order();
        let mut changed = false;

        for &caller_idx in &order {
            if inline_calls_in_function(module, caller_idx, &cg, self.threshold) {
                changed = true;
            }
        }

        changed
    }
}

fn inline_calls_in_function(
    module: &mut Module,
    caller_idx: u32,
    cg: &CallGraph,
    threshold: usize,
) -> bool {
    // Find call sites eligible for inlining.
    let call_sites: Vec<(BlockId, usize, u32, Vec<ValueId>)> = {
        let caller = &module.functions[caller_idx as usize];
        let mut sites = Vec::new();
        for block in &caller.blocks {
            for (inst_idx, inst) in block.insts.iter().enumerate() {
                if let InstKind::Call(FuncRef::Internal(callee_idx), args) = &inst.kind {
                    let ci = *callee_idx;
                    // Don't inline recursive functions.
                    if cg.is_recursive(ci) {
                        continue;
                    }
                    // Don't self-inline.
                    if ci == caller_idx {
                        continue;
                    }
                    // Cost check.
                    if cg.inline_cost(ci) > threshold {
                        continue;
                    }
                    // Argument/parameter type agreement.  When a
                    // Fortran OPTIONAL parameter is absent at a call
                    // site, the caller passes `const_i64 0` as a null
                    // placeholder — that's sound at the call boundary
                    // because the callee wraps every read in
                    // PRESENT(), but inlining would map a `load %b`
                    // from the callee onto a `load <i64 const>` in the
                    // caller, which fails the IR verifier.  Refuse to
                    // inline any call whose arg type doesn't match the
                    // callee param type exactly.  The same check also
                    // guards any future call-boundary coercion shims.
                    let callee = &module.functions[ci as usize];
                    if callee.params.len() != args.len() {
                        continue;
                    }
                    let mismatched =
                        callee.params.iter().zip(args.iter()).any(|(p, a)| {
                            match caller.value_type(*a) {
                                Some(ty) => ty != p.ty,
                                None => true,
                            }
                        });
                    if mismatched {
                        continue;
                    }
                    sites.push((block.id, inst_idx, ci, args.clone()));
                }
            }
        }
        sites
    };

    if call_sites.is_empty() {
        return false;
    }

    // Inline one call site, then return true to let the pass manager
    // re-run us. Processing multiple sites in one invocation is unsafe:
    // splitting a block invalidates indices for other call sites in the
    // same block. The pass manager's fixpoint loop handles re-invocation.
    let (call_block_id, call_inst_idx, callee_idx, caller_args) = call_sites[0].clone();
    {
        // Clone the callee's body into the caller.
        let callee = &module.functions[callee_idx as usize];
        let callee_entry = callee.entry;
        let callee_blocks: Vec<BasicBlock> = callee.blocks.clone();
        let callee_params: Vec<Param> = callee.params.clone();
        let callee_return_ty = callee.return_type.clone();

        let caller = &mut module.functions[caller_idx as usize];

        // Build value map: callee params → caller args.
        let mut val_map: HashMap<ValueId, ValueId> = HashMap::new();
        for (param, &arg) in callee_params.iter().zip(caller_args.iter()) {
            val_map.insert(param.id, arg);
        }

        // Allocate fresh IDs for all callee values.
        let mut block_map: HashMap<BlockId, BlockId> = HashMap::new();
        for cb in &callee_blocks {
            let new_bid = caller.create_block(&format!("inline_{}", cb.name));
            block_map.insert(cb.id, new_bid);
        }

        // Create post-call block to receive the return value.
        let post_call = caller.create_block("inline_post");
        let has_return_val = !matches!(callee_return_ty, IrType::Void);

        let result_param_id = if has_return_val {
            let pid = caller.next_value_id();
            caller.register_type(pid, callee_return_ty.clone());
            caller.block_mut(post_call).params.push(BlockParam {
                id: pid,
                ty: callee_return_ty.clone(),
            });
            Some(pid)
        } else {
            None
        };

        // Clone block params and pre-allocate fresh IDs for every
        // callee instruction before remapping any instruction bodies.
        // Valid SSA can use a value defined in a dominating block that
        // appears later in the block vector; remapping instruction
        // operands against a partial map leaves raw callee IDs behind
        // and can alias unrelated caller values.
        for cb in &callee_blocks {
            let new_bid = block_map[&cb.id];
            // Clone block params.
            for bp in &cb.params {
                let new_id = caller.next_value_id();
                caller.register_type(new_id, bp.ty.clone());
                val_map.insert(bp.id, new_id);
                caller.block_mut(new_bid).params.push(BlockParam {
                    id: new_id,
                    ty: bp.ty.clone(),
                });
            }
        }
        for cb in &callee_blocks {
            for inst in &cb.insts {
                let new_id = caller.next_value_id();
                caller.register_type(new_id, inst.ty.clone());
                val_map.insert(inst.id, new_id);
            }
        }
        for cb in &callee_blocks {
            let new_bid = block_map[&cb.id];
            for inst in &cb.insts {
                let new_id = *val_map
                    .get(&inst.id)
                    .expect("cloned callee instruction id should be preallocated");
                let new_kind = remap_inst_kind(&inst.kind, &val_map);
                caller.block_mut(new_bid).insts.push(Inst {
                    id: new_id,
                    kind: new_kind,
                    ty: inst.ty.clone(),
                    span: inst.span,
                });
            }
        }

        // Clone terminators, replacing Return with Branch to post_call.
        for cb in &callee_blocks {
            let new_bid = block_map[&cb.id];
            let new_term = match &cb.terminator {
                Some(Terminator::Return(Some(val))) => {
                    let remapped = *val_map.get(val).unwrap_or(val);
                    Terminator::Branch(post_call, vec![remapped])
                }
                Some(Terminator::Return(None)) => Terminator::Branch(post_call, vec![]),
                Some(other) => remap_terminator(other, &block_map, &val_map),
                None => Terminator::Unreachable,
            };
            caller.block_mut(new_bid).terminator = Some(new_term);
        }

        // Split the call-containing block: move instructions after the call
        // into the post-call block.
        let call_block = caller.block_mut(call_block_id);
        let call_result_id = call_block.insts[call_inst_idx].id;

        // Move post-call instructions to the new block.
        let post_insts: Vec<Inst> = call_block.insts.split_off(call_inst_idx + 1);
        let old_term = call_block.terminator.take();

        // Remove the call instruction itself.
        call_block.insts.pop(); // removes the call at call_inst_idx

        // Add branch from call block to inlined entry.
        let inlined_entry = block_map[&callee_entry];
        caller.block_mut(call_block_id).terminator =
            Some(Terminator::Branch(inlined_entry, vec![]));

        // Populate post-call block with remaining instructions and terminator.
        // Remap uses of the call result to the post-call block param.
        let mut post_remap: HashMap<ValueId, ValueId> = HashMap::new();
        if let Some(param_id) = result_param_id {
            post_remap.insert(call_result_id, param_id);
        }

        for inst in post_insts {
            let new_kind = if post_remap.is_empty() {
                inst.kind.clone()
            } else {
                remap_inst_kind(&inst.kind, &post_remap)
            };
            caller.block_mut(post_call).insts.push(Inst {
                id: inst.id,
                kind: new_kind,
                ty: inst.ty,
                span: inst.span,
            });
        }

        if let Some(term) = old_term {
            let new_term = if post_remap.is_empty() {
                term
            } else {
                remap_terminator(&term, &HashMap::new(), &post_remap)
            };
            caller.block_mut(post_call).terminator = Some(new_term);
        }

        if let Some(param_id) = result_param_id {
            substitute_uses(caller, call_result_id, param_id);
        }
    } // end single inline

    let caller = &mut module.functions[caller_idx as usize];
    prune_unreachable(caller);
    true
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IntWidth, IrType};
    use crate::ir::verify::verify_module;
    use crate::lexer::{Position, Span};
    use crate::opt::pass::Pass;

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
    fn inline_no_op_at_o0() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("main".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = Inline::for_level(OptLevel::O0);
        assert!(!pass.run(&mut m));
    }

    #[test]
    fn inline_no_op_without_internal_calls() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);
        let mut f = Function::new("main".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = Inline::for_level(OptLevel::O2);
        assert!(!pass.run(&mut m));
    }

    #[test]
    fn inline_rewrites_result_uses_in_successor_blocks() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);

        let mut callee = Function::new("callee".into(), vec![], IrType::Int(IntWidth::I32));
        let then_b = callee.create_block("then");
        let else_b = callee.create_block("else");
        let cond = push(&mut callee, InstKind::ConstBool(true), IrType::Bool);
        let entry = callee.entry;
        callee.block_mut(entry).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: then_b,
            true_args: vec![],
            false_dest: else_b,
            false_args: vec![],
        });
        let one = callee.next_value_id();
        callee.register_type(one, IrType::Int(IntWidth::I32));
        callee.block_mut(then_b).insts.push(Inst {
            id: one,
            kind: InstKind::ConstInt(1, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        callee.block_mut(then_b).terminator = Some(Terminator::Return(Some(one)));
        let two = callee.next_value_id();
        callee.register_type(two, IrType::Int(IntWidth::I32));
        callee.block_mut(else_b).insts.push(Inst {
            id: two,
            kind: InstKind::ConstInt(2, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        callee.block_mut(else_b).terminator = Some(Terminator::Return(Some(two)));
        m.add_function(callee);

        let mut caller = Function::new("caller".into(), vec![], IrType::Int(IntWidth::I32));
        let then_b = caller.create_block("then");
        let else_b = caller.create_block("else");
        let call_id = push(
            &mut caller,
            InstKind::Call(FuncRef::Internal(0), vec![]),
            IrType::Int(IntWidth::I32),
        );
        let zero = push(
            &mut caller,
            InstKind::ConstInt(0, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let cond = push(
            &mut caller,
            InstKind::ICmp(CmpOp::Gt, call_id, zero),
            IrType::Bool,
        );
        let entry = caller.entry;
        caller.block_mut(entry).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: then_b,
            true_args: vec![],
            false_dest: else_b,
            false_args: vec![],
        });
        let add = caller.next_value_id();
        caller.register_type(add, IrType::Int(IntWidth::I32));
        caller.block_mut(then_b).insts.push(Inst {
            id: add,
            kind: InstKind::IAdd(call_id, zero),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        caller.block_mut(then_b).terminator = Some(Terminator::Return(Some(add)));
        caller.block_mut(else_b).terminator = Some(Terminator::Return(Some(call_id)));
        m.add_function(caller);

        let pass = Inline::for_level(OptLevel::O2);
        assert!(pass.run(&mut m), "expected the internal call to inline");

        let post = verify_module(&m);
        assert!(
            post.is_empty(),
            "inliner left invalid SSA when the call result escaped into successor blocks: {:?}",
            post
        );
    }

    #[test]
    fn inline_preallocates_defs_for_later_vector_blocks() {
        let mut m = Module::new("test".into(), crate::target::TargetLayout::LP64);

        let mut callee = Function::new("callee".into(), vec![], IrType::Int(IntWidth::I32));
        let use_b = callee.create_block("use");
        let def_b = callee.create_block("def");
        let entry = callee.entry;
        callee.block_mut(entry).terminator = Some(Terminator::Branch(def_b, vec![]));

        let shared = callee.next_value_id();
        callee.register_type(shared, IrType::Int(IntWidth::I32));
        callee.block_mut(def_b).insts.push(Inst {
            id: shared,
            kind: InstKind::ConstInt(7, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        callee.block_mut(def_b).terminator = Some(Terminator::Branch(use_b, vec![]));

        let doubled = callee.next_value_id();
        callee.register_type(doubled, IrType::Int(IntWidth::I32));
        callee.block_mut(use_b).insts.push(Inst {
            id: doubled,
            kind: InstKind::IAdd(shared, shared),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        callee.block_mut(use_b).terminator = Some(Terminator::Return(Some(doubled)));
        m.add_function(callee);

        let mut caller = Function::new("caller".into(), vec![], IrType::Int(IntWidth::I32));
        let call_id = push(
            &mut caller,
            InstKind::Call(FuncRef::Internal(0), vec![]),
            IrType::Int(IntWidth::I32),
        );
        let entry = caller.entry;
        caller.block_mut(entry).terminator = Some(Terminator::Return(Some(call_id)));
        m.add_function(caller);

        let pass = Inline::for_level(OptLevel::O2);
        assert!(pass.run(&mut m), "expected the internal call to inline");

        let post = verify_module(&m);
        assert!(
            post.is_empty(),
            "inliner left invalid IDs when the callee block vector was not in dominance order: {:?}",
            post
        );
    }
}
