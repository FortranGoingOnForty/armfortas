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
//! `Load` is conservatively *not* hoisted here — proving a load is
//! invariant requires showing no `Store`/`Call`/`RuntimeCall` in the
//! loop can write the same address. Alias analysis lands later;
//! until then, leaving loads in place is correct.
//!
//! `Alloca` is also not hoisted: each iteration's stack slot might be
//! semantically distinct (in our IR allocas are entry-block-only in
//! practice, but we don't bet on that here).

use super::pass::Pass;
use super::util::{find_natural_loops, predecessors, inst_uses, NaturalLoop};
use crate::ir::inst::*;
use std::collections::{HashMap, HashSet};

/// True if an instruction is a candidate for LICM (pure, no aliasing
/// concerns, no side effects, **and safe to speculate**).
///
/// Audit Med-6: trap-prone arithmetic must not be hoisted out of a
/// guarding conditional. The classic case is `if (b /= 0) c = a / b`
/// inside a loop — once mem2reg lets LICM see real invariants, the
/// `idiv a, b` looks invariant and pure, but hoisting it past the
/// guard turns a safe program into a SIGFPE. We exclude every
/// integer/float op that can trap or produce a domain error.
fn is_hoist_candidate(kind: &InstKind) -> bool {
    !matches!(
        kind,
        InstKind::Load(..)
            | InstKind::Store(..)
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

/// Find the unique preheader for a natural loop, if one exists.
///
/// Returns `Some(preheader_id)` only when:
///  * the header has exactly one predecessor outside the loop body,
///  * that predecessor's terminator is an unconditional `Branch` to
///    the header (so it has no other live successors), and
///  * the preheader is not itself the header.
fn find_preheader(
    func: &Function,
    lp: &NaturalLoop,
    preds: &HashMap<BlockId, Vec<BlockId>>,
) -> Option<BlockId> {
    let header_preds = preds.get(&lp.header)?;
    let outside: Vec<BlockId> = header_preds
        .iter()
        .copied()
        .filter(|p| !lp.body.contains(p))
        .collect();
    if outside.len() != 1 { return None; }
    let ph = outside[0];
    if ph == lp.header { return None; }
    let ph_block = func.block(ph);
    match &ph_block.terminator {
        Some(Terminator::Branch(dest, _)) if *dest == lp.header => Some(ph),
        _ => None,
    }
}

/// Build the set of `ValueId`s defined inside the loop body. A value
/// counts as "loop-defined" if it's a block param of any body block or
/// the result of an instruction in any body block.
fn loop_defined_values(func: &Function, lp: &NaturalLoop) -> HashSet<ValueId> {
    let mut defs = HashSet::new();
    for block in &func.blocks {
        if !lp.body.contains(&block.id) { continue; }
        for bp in &block.params { defs.insert(bp.id); }
        for inst in &block.insts { defs.insert(inst.id); }
    }
    defs
}

/// One hoist directive: move instruction at (block_idx, inst_idx) into
/// the preheader.
struct Hoist {
    block_idx: usize,
    inst_idx: usize,
}

/// Run LICM on one function. Returns true if anything was hoisted.
fn licm_function(func: &mut Function) -> bool {
    let loops = find_natural_loops(func);
    if loops.is_empty() { return false; }

    let preds = predecessors(func);
    let mut any_hoisted = false;

    // Stable mapping from BlockId to current vector index — needed
    // because once we move instructions between blocks, indices shift.
    // We rebuild this map at the start of each loop iteration.
    let mut block_index: HashMap<BlockId, usize> = HashMap::new();
    for (i, b) in func.blocks.iter().enumerate() {
        block_index.insert(b.id, i);
    }

    for lp in &loops {
        // Need a preheader to hoist into.
        let Some(ph_id) = find_preheader(func, lp, &preds) else { continue; };

        // Iteratively find invariant instructions until a full pass
        // turns up nothing new. After each round we mark the hoisted
        // instructions as no longer "loop-defined" so subsequent
        // candidates can become eligible.
        let mut loop_defs = loop_defined_values(func, lp);

        // Per-iteration: rebuild the block index map (instruction
        // moves don't change BlockIds, but they're stable anyway).
        loop {
            let mut hoists: Vec<Hoist> = Vec::new();
            for (bi, block) in func.blocks.iter().enumerate() {
                if !lp.body.contains(&block.id) { continue; }
                for (ii, inst) in block.insts.iter().enumerate() {
                    if !is_hoist_candidate(&inst.kind) { continue; }
                    if !loop_defs.contains(&inst.id) {
                        // Already hoisted (we mark it removed from
                        // loop_defs after a successful hoist).
                        continue;
                    }
                    // All operands must be outside the loop OR already
                    // not in the loop_defs set (i.e., previously
                    // hoisted, or defined outside).
                    let operands = inst_uses(&inst.kind);
                    if operands.iter().any(|v| loop_defs.contains(v)) { continue; }
                    hoists.push(Hoist { block_idx: bi, inst_idx: ii });
                }
            }
            if hoists.is_empty() { break; }

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
    fn name(&self) -> &'static str { "licm" }

    fn run(&self, module: &mut Module) -> bool {
        let mut changed = false;
        for func in &mut module.functions {
            if licm_function(func) { changed = true; }
        }
        changed
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ir::types::{IrType, IntWidth};
    use crate::lexer::{Span, Position};

    fn dummy_span() -> Span {
        let p = Position { line: 1, col: 1 };
        Span { start: p, end: p, file_id: 0 }
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
        f.block_mut(header).params.push(BlockParam { id: i_param, ty: IrType::Int(IntWidth::I32) });
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
        f.block_mut(latch).params.push(BlockParam { id: i_in, ty: IrType::Int(IntWidth::I32) });
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
        let in_entry_const5 = entry_block.insts.iter().any(|i| matches!(i.kind, InstKind::ConstInt(5, IntWidth::I32)));
        assert!(in_entry_const5, "const(5) should be hoisted into preheader (entry)");

        let header_block = f.block(header);
        let in_header_const5 = header_block.insts.iter().any(|i| matches!(i.kind, InstKind::ConstInt(5, IntWidth::I32)));
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
        let header_has_iadd = header_block.insts.iter().any(|i| matches!(i.kind, InstKind::IAdd(..)));
        assert!(header_has_iadd, "IAdd(i_param, k) must remain in the header");
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
    fn does_not_hoist_load() {
        // Load is conservatively non-hoistable.
        let mut m = Module::new("t".into());
        let mut f = Function::new("f".into(), vec![], IrType::Void);

        // Build a loop with a Load inside it.
        let addr = f.next_value_id();
        let entry = f.entry;
        f.block_mut(entry).insts.push(Inst {
            id: addr,
            kind: InstKind::Alloca(IrType::Int(IntWidth::I32)),
            ty: IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))),
            span: dummy_span(),
        });

        let header = f.create_block("header");
        let i_param = f.next_value_id();
        f.block_mut(header).params.push(BlockParam { id: i_param, ty: IrType::Int(IntWidth::I32) });
        let v = f.next_value_id();
        f.block_mut(header).insts.push(Inst {
            id: v,
            kind: InstKind::Load(addr),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        let exit = f.create_block("exit");
        f.block_mut(exit).terminator = Some(Terminator::Return(None));
        f.block_mut(header).terminator = Some(Terminator::Branch(header, vec![i_param]));
        let init = f.next_value_id();
        f.block_mut(entry).insts.push(Inst {
            id: init,
            kind: InstKind::ConstInt(0, IntWidth::I32),
            ty: IrType::Int(IntWidth::I32),
            span: dummy_span(),
        });
        f.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![init]));

        m.add_function(f);

        assert!(!Licm.run(&mut m), "Load should not be hoisted");
    }
}
