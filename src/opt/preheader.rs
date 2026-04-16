//! Preheader insertion pass.
//!
//! For each natural loop that lacks a unique preheader, creates one by
//! inserting a new block between the out-of-loop predecessors and the
//! header. Block arguments are threaded through so SSA form is preserved.
//!
//! Run early at O2+ so downstream passes (LICM, unswitching, interchange)
//! can assume every loop has a preheader.

use super::loop_utils::find_preheader;
use super::pass::Pass;
use crate::ir::inst::*;
use crate::ir::walk::{find_natural_loops, predecessors};

/// Preheader insertion pass.
pub struct PreheaderInsert;

impl Pass for PreheaderInsert {
    fn name(&self) -> &'static str {
        "preheader-insert"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if insert_preheaders(func) {
                changed = true;
            }
        }
        changed
    }
}

/// Insert preheaders for all loops in one function.
fn insert_preheaders(func: &mut Function) -> bool {
    let mut changed = false;

    // Re-discover loops and predecessors each iteration since we mutate
    // the CFG. The pass runs at most once per loop (idempotent).
    let loops = find_natural_loops(func);
    let preds = predecessors(func);

    for lp in &loops {
        // Skip if a preheader already exists.
        if find_preheader(func, lp, &preds).is_some() {
            continue;
        }

        // Collect out-of-loop predecessors of the header.
        let header_preds = match preds.get(&lp.header) {
            Some(p) => p,
            None => continue,
        };
        let mut outside: Vec<BlockId> = header_preds
            .iter()
            .copied()
            .filter(|p| !lp.body.contains(p))
            .collect();
        outside.sort_by_key(|b| b.0);
        outside.dedup();

        if outside.is_empty() {
            continue;
        }

        // Create the preheader block. It receives the same block params
        // as the header and unconditionally branches to the header,
        // passing them through.
        let hdr = func.block(lp.header);
        let hdr_params: Vec<BlockParam> = hdr.params.clone();
        let ph_id = func.create_block("preheader");

        // Add matching block params to the preheader.
        let mut ph_param_ids = Vec::new();
        for bp in &hdr_params {
            let new_id = func.next_value_id();
            func.register_type(new_id, bp.ty.clone());
            func.block_mut(ph_id).params.push(BlockParam {
                id: new_id,
                ty: bp.ty.clone(),
            });
            ph_param_ids.push(new_id);
        }

        // Preheader terminates with unconditional branch to header,
        // passing its params through.
        func.block_mut(ph_id).terminator = Some(Terminator::Branch(lp.header, ph_param_ids));

        // Redirect each out-of-loop predecessor to branch to the
        // preheader instead of the header.
        let header = lp.header;
        for &pred_id in &outside {
            redirect_terminator(func, pred_id, header, ph_id);
        }

        changed = true;
    }

    changed
}

/// Rewrite all edges from `block` that target `old_dest` to target
/// `new_dest` instead. Branch arguments are preserved.
fn redirect_terminator(func: &mut Function, block: BlockId, old_dest: BlockId, new_dest: BlockId) {
    let blk = func.block_mut(block);
    let term = match &mut blk.terminator {
        Some(t) => t,
        None => return,
    };
    match term {
        Terminator::Branch(dest, _) if *dest == old_dest => {
            *dest = new_dest;
        }
        Terminator::CondBranch {
            true_dest,
            false_dest,
            ..
        } => {
            if *true_dest == old_dest {
                *true_dest = new_dest;
            }
            if *false_dest == old_dest {
                *false_dest = new_dest;
            }
        }
        Terminator::Switch { default, cases, .. } => {
            if *default == old_dest {
                *default = new_dest;
            }
            for (_, dest) in cases.iter_mut() {
                if *dest == old_dest {
                    *dest = new_dest;
                }
            }
        }
        _ => {}
    }
}

// ---------------------------------------------------------------------------
// Tests
// ---------------------------------------------------------------------------

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IntWidth, IrType};
    use crate::ir::walk::predecessors;
    use crate::lexer::{Position, Span};

    fn span() -> Span {
        let pos = Position { line: 0, col: 0 };
        Span {
            file_id: 0,
            start: pos,
            end: pos,
        }
    }

    /// Build a function where the loop header has TWO out-of-loop
    /// predecessors (entry and an if_else block), so no preheader exists.
    fn build_two_entry_loop() -> Module {
        let mut m = Module::new("test".into());
        let mut f = Function::new("test".into(), vec![], IrType::Void);

        let header = f.create_block("header");
        let body = f.create_block("body");
        let latch = f.create_block("latch");
        let exit = f.create_block("exit");
        let alt = f.create_block("alt_entry");
        let entry = f.entry;

        // Entry: const 1, condBr → header(1) or alt_entry
        let c1 = f.next_value_id();
        f.register_type(c1, IrType::Int(IntWidth::I32));
        f.block_mut(entry).insts.push(Inst {
            id: c1,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let cond = f.next_value_id();
        f.register_type(cond, IrType::Bool);
        f.block_mut(entry).insts.push(Inst {
            id: cond,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ConstBool(true),
        });
        f.block_mut(entry).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: header,
            true_args: vec![c1],
            false_dest: alt,
            false_args: vec![],
        });

        // Alt entry: const 5, br header(5)
        let c5 = f.next_value_id();
        f.register_type(c5, IrType::Int(IntWidth::I32));
        f.block_mut(alt).insts.push(Inst {
            id: c5,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::ConstInt(5, IntWidth::I32),
        });
        f.block_mut(alt).terminator = Some(Terminator::Branch(header, vec![c5]));

        // Header: block param, icmp, condBr body/exit
        let iv = f.next_value_id();
        f.register_type(iv, IrType::Int(IntWidth::I32));
        f.block_mut(header).params.push(BlockParam {
            id: iv,
            ty: IrType::Int(IntWidth::I32),
        });
        let c10 = f.next_value_id();
        f.register_type(c10, IrType::Int(IntWidth::I32));
        f.block_mut(header).insts.push(Inst {
            id: c10,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::ConstInt(10, IntWidth::I32),
        });
        let cmp = f.next_value_id();
        f.register_type(cmp, IrType::Bool);
        f.block_mut(header).insts.push(Inst {
            id: cmp,
            ty: IrType::Bool,
            span: span(),
            kind: InstKind::ICmp(CmpOp::Le, iv, c10),
        });
        f.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond: cmp,
            true_dest: body,
            true_args: vec![],
            false_dest: exit,
            false_args: vec![],
        });

        // Body → latch
        f.block_mut(body).terminator = Some(Terminator::Branch(latch, vec![]));

        // Latch: iadd + br header
        let one_l = f.next_value_id();
        f.register_type(one_l, IrType::Int(IntWidth::I32));
        f.block_mut(latch).insts.push(Inst {
            id: one_l,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::ConstInt(1, IntWidth::I32),
        });
        let nxt = f.next_value_id();
        f.register_type(nxt, IrType::Int(IntWidth::I32));
        f.block_mut(latch).insts.push(Inst {
            id: nxt,
            ty: IrType::Int(IntWidth::I32),
            span: span(),
            kind: InstKind::IAdd(iv, one_l),
        });
        f.block_mut(latch).terminator = Some(Terminator::Branch(header, vec![nxt]));

        // Exit
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        m.add_function(f);
        m
    }

    #[test]
    fn inserts_preheader_for_multi_entry_loop() {
        let mut m = build_two_entry_loop();
        let f = &m.functions[0];

        // Before: header has 2 out-of-loop preds (entry, alt_entry).
        let loops = find_natural_loops(f);
        let preds = predecessors(f);
        assert_eq!(loops.len(), 1);
        assert!(
            find_preheader(f, &loops[0], &preds).is_none(),
            "should have no preheader before insertion"
        );

        let pass = PreheaderInsert;
        let changed = pass.run(&mut m);
        assert!(changed, "should have inserted a preheader");

        // After: header should have exactly one out-of-loop pred.
        let f = &m.functions[0];
        let loops = find_natural_loops(f);
        let preds = predecessors(f);
        let ph = find_preheader(f, &loops[0], &preds);
        assert!(ph.is_some(), "preheader should now exist");

        // The preheader should have a block param matching the header's.
        let ph_block = f.block(ph.unwrap());
        assert_eq!(ph_block.params.len(), 1, "preheader should have 1 param");
    }

    #[test]
    fn idempotent_when_preheader_exists() {
        let mut m = build_two_entry_loop();
        let pass = PreheaderInsert;
        pass.run(&mut m);
        let block_count_after_first = m.functions[0].blocks.len();

        // Running again should not change anything.
        let changed = pass.run(&mut m);
        assert!(!changed, "second run should be idempotent");
        assert_eq!(m.functions[0].blocks.len(), block_count_after_first);
    }
}
