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

use std::collections::HashMap;
use crate::ir::inst::*;
use crate::ir::types::IrType;
use crate::ir::walk::prune_unreachable;
use super::callgraph::CallGraph;
use super::loop_utils::{remap_inst_kind, remap_terminator};
use super::pass::Pass;
use super::pipeline::OptLevel;

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
    fn name(&self) -> &'static str { "inline" }

    fn run(&self, module: &mut Module) -> bool {
        if self.threshold == 0 { return false; }

        let cg = CallGraph::build(module);
        let order = cg.reverse_postorder();
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
                    if cg.is_recursive(ci) { continue; }
                    // Don't self-inline.
                    if ci == caller_idx { continue; }
                    // Cost check.
                    if cg.inline_cost(ci) > threshold { continue; }
                    sites.push((block.id, inst_idx, ci, args.clone()));
                }
            }
        }
        sites
    };

    if call_sites.is_empty() { return false; }

    // Process all call sites in reverse order. Each inline appends new
    // blocks at the end and splits the call-containing block; processing
    // in reverse preserves indices for earlier (lower-index) sites.
    let mut any_inlined = false;
    for &(call_block_id, call_inst_idx, callee_idx, ref caller_args) in call_sites.iter().rev() {
        let caller_args = caller_args.clone();

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

    // Clone block params and instructions.
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
        // Clone instructions.
        for inst in &cb.insts {
            let new_id = caller.next_value_id();
            caller.register_type(new_id, inst.ty.clone());
            val_map.insert(inst.id, new_id);
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
            Some(Terminator::Return(None)) => {
                Terminator::Branch(post_call, vec![])
            }
            Some(other) => {
                remap_terminator(other, &block_map, &val_map)
            }
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

        any_inlined = true;
    } // end for call_sites

    if any_inlined {
        let caller = &mut module.functions[caller_idx as usize];
        prune_unreachable(caller);
    }
    any_inlined
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IrType, IntWidth};
    use crate::opt::pass::Pass;

    #[test]
    fn inline_no_op_at_o0() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("main".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = Inline::for_level(OptLevel::O0);
        assert!(!pass.run(&mut m));
    }

    #[test]
    fn inline_no_op_without_internal_calls() {
        let mut m = Module::new("test".into());
        let mut f = Function::new("main".into(), vec![], IrType::Void);
        f.block_mut(f.entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);
        let pass = Inline::for_level(OptLevel::O2);
        assert!(!pass.run(&mut m));
    }
}
