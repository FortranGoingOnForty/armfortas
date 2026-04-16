//! Loop-invariant code motion.
//!
//! For each natural loop, hoist instructions whose operands are all
//! defined *outside* the loop into a preheader. This is the classic
//! win for tight Fortran kernels — a constant address calculation, a
//! type-tag fetch, or a runtime helper call lifts out of the inner
//! body and runs once.
//!
//! ## Preheader requirement
//!
//! Hoisted instructions need somewhere to live that **executes exactly
//! once before the loop and dominates the header**. The standard name
//! for this block is a *preheader*. We require the loop already to
//! have one — i.e., the header has exactly one out-of-loop predecessor
//! and that predecessor's only successor is the header. This is the
//! shape Fortran `do` lowering naturally produces (entry block →
//! header). When the property doesn't hold we skip the loop rather
//! than synthesize a preheader; preheader insertion is its own pass
//! that we'll add when LICM is paying for itself.
//!
//! ## Invariance check
//!
//! An instruction `I` inside the loop is loop-invariant when it is
//! **pure** (no side effects, not a memory or call op) and every
//! operand of `I` is defined outside the loop body — either in a
//! dominating block, as a function parameter, or as a previously
//! hoisted instruction now living in the preheader. We discover this
//! iteratively by sweeping the body until a full pass turns up
//! nothing new.
//!
//! Block parameters of the header carry per-iteration values (like
//! the induction variable), so anything depending on a header param
//! is *never* invariant. Block parameters of other blocks in the loop
//! are likewise loop-defined.
//!
//! ## Memory operations
//!
//! `Load` is hoisted only when its address is loop-invariant and every
//! loop `Store` is proven `NoAlias` against that address. Any
//! `Call`/`RuntimeCall` in the loop still blocks hoisting for now.
//!
//! `Alloca` is also not hoisted: each iteration's stack slot might be
//! semantically distinct (in our IR allocas are entry-block-only in
//! practice, but we don't bet on that here).

use super::alias::{self, AliasResult};
use super::pass::Pass;
use super::util::{find_natural_loops, inst_uses, predecessors, NaturalLoop};
use crate::ir::inst::*;
use std::collections::{HashMap, HashSet};

/// True if an instruction is a non-memory candidate for LICM (pure,
/// no side effects, **and safe to speculate**).
///
/// Audit Med-6: trap-prone arithmetic must not be hoisted out of a
/// guarding conditional. The classic case is `if (b /= 0) c = a / b`
/// inside a loop — once mem2reg lets LICM see real invariants, the
/// `idiv a, b` looks invariant and pure, but hoisting it past the
/// guard turns a safe program into a SIGFPE. We exclude every
/// integer/float op that can trap or produce a domain error.
fn is_non_memory_hoist_candidate(kind: &InstKind) -> bool {
    !matches!(
        kind,
        InstKind::Store(..)
            | InstKind::Alloca(..)
            | InstKind::Call(..)
            | InstKind::RuntimeCall(..)
            | InstKind::ConstString(..)
            | InstKind::Undef(..)
            // Trap-prone pure operations — never speculate.
            | InstKind::IDiv(..)
            | InstKind::IMod(..)
            | InstKind::FDiv(..)
            | InstKind::FSqrt(..)
            | InstKind::FPow(..)
    )
}

fn load_is_loop_invariant(
    func: &Function,
    lp: &NaturalLoop,
    load_id: ValueId,
    load_ptr: ValueId,
) -> bool {
    for block in &func.blocks {
        if !lp.body.contains(&block.id) {
            continue;
        }
        for inst in &block.insts {
            if inst.id == load_id {
                continue;
            }
            match &inst.kind {
                InstKind::Store(_, ptr)
                    if !matches!(alias::query(func, *ptr, load_ptr), AliasResult::NoAlias) =>
                {
                    return false;
                }
                InstKind::Call(..) | InstKind::RuntimeCall(..) => return false,
                _ => {}
            }
        }
    }
    true
}

/// Delegate to shared loop utility.
fn find_preheader(
    func: &Function,
    lp: &NaturalLoop,
    preds: &HashMap<BlockId, Vec<BlockId>>,
) -> Option<BlockId> {
    super::loop_utils::find_preheader(func, lp, preds)
}

/// Delegate to shared loop utility.
fn loop_defined_values(func: &Function, lp: &NaturalLoop) -> HashSet<ValueId> {
    super::loop_utils::loop_defined_values(func, lp)
}

/// One hoist directive: move instruction at (block_idx, inst_idx) into
/// the preheader.
struct Hoist {
    block_idx: usize,
    inst_idx: usize,
}

/// Run LICM on one function. Returns true if anything was hoisted.
fn licm_function(func: &mut Function) -> bool {
    // Audit N-6 (originally load-bearing, now defensive): drop
    // unreachable blocks before running loop analysis. Earlier
    // versions of `compute_dominators` assigned "all blocks dominate
    // me" to nodes with no predecessors, which made unreachable
    // components produce phantom natural loops. That bug was later
    // fixed at the source in `ir::walk::compute_dominators` (audit
    // M4-3), so the pre-prune is no longer *required* for
    // correctness — it's kept as cheap defense-in-depth so LICM
    // never touches code that can't execute, and so follow-up
    // passes see a tidy CFG.
    let pruned = super::util::prune_unreachable(func);

    let loops = find_natural_loops(func);
    if loops.is_empty() {
        return pruned;
    }

    let preds = predecessors(func);
    let mut any_hoisted = pruned;

    // Stable mapping from BlockId to vector index. LICM never adds,
    // removes, or reorders blocks (it only moves instructions between
    // existing blocks), so this map is built once and remains valid
    // for the entire pass. Audit Min-2: an earlier comment claimed
    // we'd "rebuild per iteration" — that was always aspirational
    // and would only matter if a future variant of LICM started
    // mutating the block vector.
    let block_index: HashMap<BlockId, usize> = func
        .blocks
        .iter()
        .enumerate()
        .map(|(i, b)| (b.id, i))
        .collect();

    for lp in &loops {
        // Need a preheader to hoist into.
        let Some(ph_id) = find_preheader(func, lp, &preds) else {
            continue;
        };

        // Iteratively find invariant instructions until a full pass
        // turns up nothing new. After each round we mark the hoisted
        // instructions as no longer "loop-defined" so subsequent
        // candidates can become eligible.
        let mut loop_defs = loop_defined_values(func, lp);

        loop {
            let mut hoists: Vec<Hoist> = Vec::new();
            for (bi, block) in func.blocks.iter().enumerate() {
                if !lp.body.contains(&block.id) {
                    continue;
                }
                for (ii, inst) in block.insts.iter().enumerate() {
                    if !loop_defs.contains(&inst.id) {
                        // Already hoisted (we mark it removed from
                        // loop_defs after a successful hoist).
                        continue;
                    }
                    // All operands must be outside the loop OR already
                    // not in the loop_defs set (i.e., previously
                    // hoisted, or defined outside).
                    let operands = inst_uses(&inst.kind);
                    if operands.iter().any(|v| loop_defs.contains(v)) {
                        continue;
                    }
                    let hoistable = match &inst.kind {
                        InstKind::Load(ptr) => load_is_loop_invariant(func, lp, inst.id, *ptr),
                        _ => is_non_memory_hoist_candidate(&inst.kind),
                    };
                    if !hoistable {
                        continue;
                    }
                    hoists.push(Hoist {
                        block_idx: bi,
                        inst_idx: ii,
                    });
                }
            }
            if hoists.is_empty() {
                break;
            }

            // Apply hoists: remove from source blocks (in reverse
            // index order so earlier indices remain valid), then
            // append to the preheader's instruction list right before
            // its terminator. We don't reorder among hoists — order
            // within the loop body matters for SSA dominance, and
            // sorting them by (bi, ii) ascending then inserting in
            // that order at the end of the preheader preserves their
            // relative ordering.
            hoists.sort_by_key(|h| (h.block_idx, h.inst_idx));

            // Bucket by source block so we can remove in reverse.
            let mut by_block: HashMap<usize, Vec<usize>> = HashMap::new();
            for h in &hoists {
                by_block.entry(h.block_idx).or_default().push(h.inst_idx);
            }

            // Extract the instructions in the original order.
            let mut extracted: Vec<Inst> = Vec::with_capacity(hoists.len());
            for h in &hoists {
                // Take the instruction (we'll remove it below).
                extracted.push(func.blocks[h.block_idx].insts[h.inst_idx].clone());
            }
            for (bi, mut idxs) in by_block {
                idxs.sort_by(|a, b| b.cmp(a));
                for ii in idxs {
                    func.blocks[bi].insts.remove(ii);
                }
            }

            // Append into the preheader, in original order, before
            // its terminator. Since the preheader's terminator lives
            // in `terminator: Option<Terminator>`, the instructions
            // simply append.
            let ph_idx = block_index[&ph_id];
            for inst in extracted.iter() {
                loop_defs.remove(&inst.id);
            }
            func.blocks[ph_idx].insts.extend(extracted);

            any_hoisted = true;
        }
    }

    any_hoisted
}

pub struct Licm;

impl Pass for Licm {
    fn name(&self) -> &'static str {
        "licm"
    }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if licm_function(func) {
                changed = true;
            }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IntWidth, IrType};
    use crate::lexer::{Position, Span};

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
            ty: ty.clone(),
            span: dummy_span(),
        });
        f.register_type(id, ty);
        id
    }

    /// Build a tiny loop:
    ///
    /// ```text
    ///   entry:
    ///     %limit = const 10
    ///     %init  = const 0
    ///     branch header(%init)
    ///   header(%i:i32):
    ///     %k     = const 5            ; loop-invariant — should hoist
    ///     %tmp   = iadd %i, %k        ; depends on i, NOT invariant
    ///     %done  = icmp Ge %i, %limit
    ///     cond_branch %done, exit, latch(%i)
    ///   latch(%i_in:i32):
    ///     %one  = const 1
    ///     %next = iadd %i_in, %one
    ///     branch header(%next)
    ///   exit:
    ///     ret
    /// ```
    fn build_loop_module() -> (Module, BlockId, BlockId, BlockId) {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        // Build entry first.
        let limit = f.next_value_id();
        let init = f.next_value_id();
        let entry = f.entry;
        f.block_mut(entry).insts.push(Inst {
            id: limit,
            kind: InstKind::ConstInt(10, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(entry).insts.push(Inst {
            id: init,
            kind: InstKind::ConstInt(0, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });

        // Header.
        let header = f.create_block("header");
        let i_param = f.next_value_id();
        f.block_mut(header).params.push(BlockParam {
            id: i_param,
            ty: IrType::Int(IntWidth::I32),
        });
        let k = f.next_value_id();
        let tmp = f.next_value_id();
        let done = f.next_value_id();
        f.block_mut(header).insts.push(Inst {
            id: k,
            kind: InstKind::ConstInt(5, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(header).insts.push(Inst {
            id: tmp,
            kind: InstKind::IAdd(i_param, k),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(header).insts.push(Inst {
            id: done,
            kind: InstKind::ICmp(CmpOp::Ge, i_param, limit),
            ty: IrType::Bool,
            span: dummy_span(),
        });

        // Latch.
        let latch = f.create_block("latch");
        let i_in = f.next_value_id();
        f.block_mut(latch).params.push(BlockParam {
            id: i_in,
            ty: IrType::Int(IntWidth::I32),
        });
        let one = f.next_value_id();
        let next = f.next_value_id();
        f.block_mut(latch).insts.push(Inst {
            id: one,
            kind: InstKind::ConstInt(1, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(latch).insts.push(Inst {
            id: next,
            kind: InstKind::IAdd(i_in, one),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(latch).terminator = Some(Terminator::Branch(header, vec![next]));

        // Exit.
        let exit = f.create_block("exit");
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        // Wire entry → header.
        f.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![init]));
        f.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond: done,
            true_dest: exit,
            true_args: vec![],
            false_dest: latch,
            false_args: vec![i_param],
        });

        m.add_function(f);
        (m, header, latch, exit)
    }

    #[test]
    fn hoists_invariant_const_into_preheader() {
        let (mut m, header, _latch, _exit) = build_loop_module();

        assert!(Licm.run(&mut m));

        let f = &m.functions[0];
        let entry_block = f.block(f.entry);
        // The const(5) should now live in the entry block (which is
        // the natural preheader), not the header.
        let in_entry_const5 = entry_block
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::ConstInt(5, IntWidth::I32)));
        assert!(
            in_entry_const5,
            "const(5) should be hoisted into preheader (entry)"
        );

        let header_block = f.block(header);
        let in_header_const5 = header_block
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::ConstInt(5, IntWidth::I32)));
        assert!(!in_header_const5, "const(5) should be removed from header");
    }

    #[test]
    fn does_not_hoist_loop_dependent() {
        // The IAdd(i_param, k) inside the loop must NOT be hoisted —
        // it depends on i_param.
        let (mut m, header, _latch, _exit) = build_loop_module();
        Licm.run(&mut m);
        let f = &m.functions[0];
        let header_block = f.block(header);
        let header_has_iadd = header_block
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::IAdd(..)));
        assert!(
            header_has_iadd,
            "IAdd(i_param, k) must remain in the header"
        );
    }

    #[test]
    fn no_loop_no_change() {
        // Straight-line function with no loop.
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);
        let _x = f.next_value_id();
        let entry = f.entry;
        f.block_mut(entry).insts.push(Inst {
            id: _x,
            kind: InstKind::ConstInt(7, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(entry).terminator = Some(Terminator::Return(None));
        m.add_function(f);

        assert!(!Licm.run(&mut m));
    }

    #[test]
    fn does_not_hoist_load_with_aliasing_store_in_loop() {
        // A load whose pointer aliases with an intervening store in
        // the same loop must NOT be hoisted: each iteration can see
        // a different value after the store.
        //
        // Build entry → header → exit, with a Load followed by a
        // Store(i_param, addr) in the header.  The alias oracle sees
        // the same pointer for both, so LICM must leave the load in
        // place.
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);

        let addr = f.next_value_id();
        let entry = f.entry;
        f.block_mut(entry).insts.push(Inst {
            id: addr,
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: dummy_span(),
        });
        let init = f.next_value_id();
        f.block_mut(entry).insts.push(Inst {
            id: init,
            kind: InstKind::ConstInt(0, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });

        let header = f.create_block("header");
        let i_param = f.next_value_id();
        f.block_mut(header).params.push(BlockParam {
            id: i_param,
            ty: IrType::Int(IntWidth::I32),
        });
        let v = f.next_value_id();
        f.block_mut(header).insts.push(Inst {
            id: v,
            kind: InstKind::Load(addr),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        // Intervening aliasing store — writes i_param into the same
        // slot the load just read.  Makes the load non-invariant.
        let store = f.next_value_id();
        f.block_mut(header).insts.push(Inst {
            id: store,
            kind: InstKind::Store(i_param, addr),
            ty: IrType::Void,
            span: dummy_span(),
        });
        let cond = f.next_value_id();
        f.block_mut(header).insts.push(Inst {
            id: cond,
            kind: InstKind::ConstBool(true),
            ty: IrType::Bool,
            span: dummy_span(),
        });

        let exit = f.create_block("exit");
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        f.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: header,
            true_args: vec![i_param],
            false_dest: exit,
            false_args: vec![],
        });
        f.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![init]));

        m.add_function(f);

        Licm.run(&mut m);

        // The Load must still be in the header after LICM.
        let f = &m.functions[0];
        let header_block = f.block(header);
        let load_in_header = header_block
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::Load(_)));
        assert!(
            load_in_header,
            "ambiguous loop load should not have been hoisted"
        );
    }

    #[test]
    fn hoists_loop_invariant_load_with_no_store_in_loop() {
        // Positive coverage: when a load's pointer has no aliasing
        // store (or call) in the loop body, the alias-aware LICM
        // oracle proves the load is loop-invariant and hoists it
        // into the preheader.  This is the mirror image of
        // does_not_hoist_load_with_aliasing_store_in_loop: remove
        // the store from the header and assert the load moved out.
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);

        let addr = f.next_value_id();
        let entry = f.entry;
        f.block_mut(entry).insts.push(Inst {
            id: addr,
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: dummy_span(),
        });
        let init = f.next_value_id();
        f.block_mut(entry).insts.push(Inst {
            id: init,
            kind: InstKind::ConstInt(0, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        // Give the slot a known value in the entry block so the
        // hoisted load doesn't read undef.  LICM's correctness does
        // not depend on this; we add it for realism.
        let seed = f.next_value_id();
        f.block_mut(entry).insts.push(Inst {
            id: seed,
            kind: InstKind::ConstInt(99, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        let entry_store = f.next_value_id();
        f.block_mut(entry).insts.push(Inst {
            id: entry_store,
            kind: InstKind::Store(seed, addr),
            ty: IrType::Void,
            span: dummy_span(),
        });

        let header = f.create_block("header");
        let i_param = f.next_value_id();
        f.block_mut(header).params.push(BlockParam {
            id: i_param,
            ty: IrType::Int(IntWidth::I32),
        });
        let v = f.next_value_id();
        f.block_mut(header).insts.push(Inst {
            id: v,
            kind: InstKind::Load(addr),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        let cond = f.next_value_id();
        f.block_mut(header).insts.push(Inst {
            id: cond,
            kind: InstKind::ConstBool(true),
            ty: IrType::Bool,
            span: dummy_span(),
        });

        let exit = f.create_block("exit");
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        f.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond,
            true_dest: header,
            true_args: vec![i_param],
            false_dest: exit,
            false_args: vec![],
        });
        f.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![init]));

        m.add_function(f);

        assert!(
            Licm.run(&mut m),
            "LICM should report that it hoisted the loop-invariant load"
        );

        // The Load must have moved out of the header — into the
        // preheader or the entry block (LICM hoists into the
        // nearest preheader, which for this single-block loop is
        // either `entry` itself or a synthesised preheader).
        let f = &m.functions[0];
        let load_still_in_header = f
            .block(header)
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::Load(_)));
        assert!(
            !load_still_in_header,
            "invariant load should have left the header"
        );
        let load_somewhere_else = f
            .blocks
            .iter()
            .filter(|b| b.id != header)
            .flat_map(|b| b.insts.iter())
            .any(|i| matches!(i.kind, InstKind::Load(_)));
        assert!(
            load_somewhere_else,
            "invariant load should have been placed in a dominating block"
        );
    }

    #[test]
    fn hoists_load_from_noalias_dummy_across_other_dummy_store() {
        let params = vec![
            Param {
                name: "a".into(),
                ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
                id: ValueId(0),
                fortran_noalias: true,
            },
            Param {
                name: "b".into(),
                ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
                id: ValueId(1),
                fortran_noalias: true,
            },
        ];
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), params, IrType::Void);

        let init = push(
            &mut f,
            InstKind::ConstInt(0, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );
        let limit = push(
            &mut f,
            InstKind::ConstInt(10, IntWidth::I32),
            IrType::Int(IntWidth::I32),
        );

        let header = f.create_block("header");
        let i_param = f.next_value_id();
        f.block_mut(header).params.push(BlockParam {
            id: i_param,
            ty: IrType::Int(IntWidth::I32),
        });

        let load_a = f.next_value_id();
        f.block_mut(header).insts.push(Inst {
            id: load_a,
            kind: InstKind::Load(ValueId(0)),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        let one = f.next_value_id();
        f.block_mut(header).insts.push(Inst {
            id: one,
            kind: InstKind::ConstInt(1, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        let store = f.next_value_id();
        f.block_mut(header).insts.push(Inst {
            id: store,
            kind: InstKind::Store(one, ValueId(1)),
            ty: IrType::Void,
            span: dummy_span(),
        });
        let next = f.next_value_id();
        f.block_mut(header).insts.push(Inst {
            id: next,
            kind: InstKind::IAdd(i_param, one),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        let done = f.next_value_id();
        f.block_mut(header).insts.push(Inst {
            id: done,
            kind: InstKind::ICmp(CmpOp::Ge, i_param, limit),
            ty: IrType::Bool,
            span: dummy_span(),
        });

        let exit = f.create_block("exit");
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        f.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond: done,
            true_dest: exit,
            true_args: vec![],
            false_dest: header,
            false_args: vec![next],
        });
        f.block_mut(f.entry).terminator = Some(Terminator::Branch(header, vec![init]));

        m.add_function(f);

        assert!(
            Licm.run(&mut m),
            "LICM should hoist the invariant dummy-arg load"
        );

        let f = &m.functions[0];
        let entry_block = f.block(f.entry);
        assert!(
            entry_block
                .insts
                .iter()
                .any(|inst| matches!(inst.kind, InstKind::Load(ValueId(0)))),
            "hoisted load should land in the loop preheader"
        );
        let header_block = f.block(header);
        assert!(
            !header_block
                .insts
                .iter()
                .any(|inst| matches!(inst.kind, InstKind::Load(ValueId(0)))),
            "loop-body load should be removed after hoisting"
        );
    }

    /// A stronger hoist test: an IMul whose operands are both
    /// defined in the entry block (so the multiply itself is loop
    /// invariant). This exercises the operand-dependency walk in
    /// LICM, not just the trivial "constant in header" case.
    #[test]
    fn hoists_invariant_imul_with_operands_from_entry() {
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);

        // Entry: build two const operands a=3, b=4 and an init value 0.
        let a = f.next_value_id();
        let bv = f.next_value_id();
        let init = f.next_value_id();
        let entry = f.entry;
        f.block_mut(entry).insts.push(Inst {
            id: a,
            kind: InstKind::ConstInt(3, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(entry).insts.push(Inst {
            id: bv,
            kind: InstKind::ConstInt(4, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(entry).insts.push(Inst {
            id: init,
            kind: InstKind::ConstInt(0, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });

        // Header: i_param, then `prod = a * b` (invariant), then
        // `tmp = i_param + prod` (loop-dependent), then a < 10 cmp.
        let header = f.create_block("header");
        let i_param = f.next_value_id();
        f.block_mut(header).params.push(BlockParam {
            id: i_param,
            ty: IrType::Int(IntWidth::I32),
        });
        let prod = f.next_value_id();
        let tmp = f.next_value_id();
        let limit = f.next_value_id();
        let done = f.next_value_id();
        f.block_mut(header).insts.push(Inst {
            id: prod,
            kind: InstKind::IMul(a, bv),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(header).insts.push(Inst {
            id: tmp,
            kind: InstKind::IAdd(i_param, prod),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(header).insts.push(Inst {
            id: limit,
            kind: InstKind::ConstInt(10, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(header).insts.push(Inst {
            id: done,
            kind: InstKind::ICmp(CmpOp::Ge, i_param, limit),
            ty: IrType::Bool,
            span: dummy_span(),
        });

        // Latch: increment i and loop back.
        let latch = f.create_block("latch");
        let i_in = f.next_value_id();
        f.block_mut(latch).params.push(BlockParam {
            id: i_in,
            ty: IrType::Int(IntWidth::I32),
        });
        let one = f.next_value_id();
        let next = f.next_value_id();
        f.block_mut(latch).insts.push(Inst {
            id: one,
            kind: InstKind::ConstInt(1, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(latch).insts.push(Inst {
            id: next,
            kind: InstKind::IAdd(i_in, one),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(latch).terminator = Some(Terminator::Branch(header, vec![next]));

        let exit = f.create_block("exit");
        f.block_mut(exit).terminator = Some(Terminator::Return(None));

        f.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![init]));
        f.block_mut(header).terminator = Some(Terminator::CondBranch {
            cond: done,
            true_dest: exit,
            true_args: vec![],
            false_dest: latch,
            false_args: vec![i_param],
        });

        m.add_function(f);

        assert!(Licm.run(&mut m), "LICM should report it changed something");
        let f = &m.functions[0];

        // The IMul must have moved out of the header into the entry
        // (the natural preheader).
        let header_block = f.block(header);
        let imul_in_header = header_block
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::IMul(..)));
        assert!(
            !imul_in_header,
            "invariant IMul should be hoisted out of the header: {:?}",
            header_block.insts
        );

        let entry_block = f.block(f.entry);
        let imul_in_entry = entry_block
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::IMul(..)));
        assert!(
            imul_in_entry,
            "invariant IMul should land in entry/preheader after hoist: {:?}",
            entry_block.insts
        );

        // The loop-dependent IAdd must NOT have moved.
        let iadd_in_header = header_block
            .insts
            .iter()
            .any(|i| matches!(i.kind, InstKind::IAdd(_, _)));
        assert!(
            iadd_in_header,
            "loop-dependent IAdd(i_param, prod) should still be in the header"
        );
    }
}
