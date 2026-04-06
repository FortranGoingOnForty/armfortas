//! Adversarial test cases generated during the mid-pipeline audit.
//!
//! Each test is named after a finding it pins down. Tests that **fail**
//! against current code expose real bugs; tests that **pass** prove the
//! audited behavior is correct so we won't regress later.

#![cfg(test)]

use crate::ir::inst::*;
use crate::ir::types::{IrType, IntWidth, FloatWidth};
use crate::lexer::{Span, Position};
use crate::ir::verify::verify_module;
use super::Pass;
use super::const_fold::ConstFold;
use super::const_prop::ConstProp;
use super::dce::Dce;
use super::strength_reduce::StrengthReduce;
use super::licm::Licm;

fn dummy_span() -> Span {
    let p = Position { line: 1, col: 1 };
    Span { start: p, end: p, file_id: 0 }
}

fn push(f: &mut Function, kind: InstKind, ty: IrType) -> ValueId {
    let id = f.next_value_id();
    let entry = f.entry;
    f.block_mut(entry).insts.push(Inst { id, kind, ty, span: dummy_span() });
    id
}

// =============================================================
// FINDING M-1: const_fold IntToFloat doesn't round to f32
// =============================================================
//
// Round-to-even at f32 precision for i32 16777217 should yield 16777216.
// Currently `signed as f64` stores the exact integer value, which is
// outside f32 mantissa precision. Downstream FCmp/FloatToInt folds will
// see a value that disagrees with what runtime would compute.
#[test]
fn audit_const_fold_int_to_f32_must_round() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Float(FloatWidth::F32));
    let i = push(&mut f, InstKind::ConstInt(16_777_217, IntWidth::I32), IrType::Int(IntWidth::I32));
    let cv = push(&mut f, InstKind::IntToFloat(i, FloatWidth::F32), IrType::Float(FloatWidth::F32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(cv)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == cv).unwrap();
    match folded.kind {
        InstKind::ConstFloat(v, FloatWidth::F32) => {
            assert_eq!(v, 16_777_216.0,
                "expected f32-precision result 16_777_216.0, got {}", v);
        }
        ref other => panic!("expected ConstFloat, got {:?}", other),
    }
}

// =============================================================
// FINDING M-2: FloatExtend / FloatTrunc preserves source precision
// =============================================================
//
// FloatTrunc to f32 should round through f32. Test it directly.
#[test]
fn audit_const_fold_float_trunc_must_round() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Float(FloatWidth::F32));
    let d = push(&mut f, InstKind::ConstFloat(16_777_217.0, FloatWidth::F64), IrType::Float(FloatWidth::F64));
    let cv = push(&mut f, InstKind::FloatTrunc(d, FloatWidth::F32), IrType::Float(FloatWidth::F32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(cv)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == cv).unwrap();
    match folded.kind {
        InstKind::ConstFloat(v, FloatWidth::F32) => {
            assert_eq!(v, 16_777_216.0, "expected f32-rounded value, got {}", v);
        }
        _ => panic!(),
    }
}

// =============================================================
// FINDING M-3: strength_reduce chained identities
// =============================================================
//
// Convince ourselves chains of identity rewrites land on the right
// terminal value. This documents the reverse-order processing.
#[test]
fn audit_strength_reduce_chained_identities() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let x  = push(&mut f, InstKind::ConstInt(7, IntWidth::I32), IrType::Int(IntWidth::I32));
    let one1 = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
    let mul1 = push(&mut f, InstKind::IMul(x, one1), IrType::Int(IntWidth::I32));
    let one2 = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
    let mul2 = push(&mut f, InstKind::IMul(mul1, one2), IrType::Int(IntWidth::I32));
    let zero = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));
    let add  = push(&mut f, InstKind::IAdd(mul2, zero), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(add)));
    m.add_function(f);

    StrengthReduce.run(&mut m);
    // After three chained identities, the return must reference x.
    let term = m.functions[0].blocks[0].terminator.as_ref().unwrap();
    match term {
        Terminator::Return(Some(v)) => assert_eq!(*v, x,
            "expected return %{} (x), got %{}", x.0, v.0),
        other => panic!("expected Return(x), got {:?}", other),
    }
    // And the IR must still verify.
    let errs = verify_module(&m);
    assert!(errs.is_empty(), "verifier errors after chained rewrites: {:?}", errs);
}

// =============================================================
// FINDING M-4: strength_reduce ShlByConst inserts new ConstInt
// in same block as a chain Identity. Verifier must still pass.
// =============================================================
#[test]
fn audit_strength_reduce_mixed_shl_and_identity_in_block() {
    // %0 = const 7
    // %1 = const 8     ; pow-of-two
    // %2 = imul %0, %1  ; will become shl
    // %3 = const 1
    // %4 = imul %2, %3  ; identity → pass through %2
    // ret %4
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let x = push(&mut f, InstKind::ConstInt(7, IntWidth::I32), IrType::Int(IntWidth::I32));
    let eight = push(&mut f, InstKind::ConstInt(8, IntWidth::I32), IrType::Int(IntWidth::I32));
    let mul1 = push(&mut f, InstKind::IMul(x, eight), IrType::Int(IntWidth::I32));
    let one = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
    let mul2 = push(&mut f, InstKind::IMul(mul1, one), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(mul2)));
    m.add_function(f);

    StrengthReduce.run(&mut m);
    let errs = verify_module(&m);
    assert!(errs.is_empty(), "verifier errors: {:?}", errs);

    // The return must transitively reach the shl (identity passed through).
    let block = &m.functions[0].blocks[0];
    let term_val = match block.terminator.as_ref().unwrap() {
        Terminator::Return(Some(v)) => *v,
        _ => panic!(),
    };
    let term_kind = &block.insts.iter().find(|i| i.id == term_val).unwrap().kind;
    assert!(matches!(term_kind, InstKind::Shl(..)),
        "expected return value to define a Shl, got {:?}", term_kind);
}

// =============================================================
// FINDING M-5: const_fold integer division narrow-width overflow
// =============================================================
//
// i8: -128 / -1 wraps to -128 in two's-complement. The fold should
// match what AArch64 SDIV produces.
#[test]
fn audit_const_fold_idiv_i8_min_neg_one() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I8));
    let a = push(&mut f, InstKind::ConstInt(-128, IntWidth::I8), IrType::Int(IntWidth::I8));
    let b = push(&mut f, InstKind::ConstInt(-1, IntWidth::I8), IrType::Int(IntWidth::I8));
    let q = push(&mut f, InstKind::IDiv(a, b), IrType::Int(IntWidth::I8));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(q)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == q).unwrap();
    // Either we fold to -128 (match hardware), or we leave the IDiv alone.
    // Anything else would be a divergence.
    match folded.kind {
        InstKind::ConstInt(v, IntWidth::I8) => assert_eq!(v, -128,
            "expected -128 (i8 wraparound), got {}", v),
        InstKind::IDiv(..) => { /* left alone, also acceptable */ }
        ref other => panic!("unexpected fold: {:?}", other),
    }
}

// =============================================================
// FINDING C-1 / Med-5: DCE removes dead block parameters and
// rewrites predecessor branch args in lockstep
// =============================================================
#[test]
fn audit_dce_removes_dead_block_param() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Void);
    let target = f.create_block("target");
    let param_id = f.next_value_id();
    f.block_mut(target).params.push(BlockParam {
        id: param_id,
        ty: IrType::Int(IntWidth::I32),
    });
    let init = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Branch(target, vec![init]));
    f.block_mut(target).terminator = Some(Terminator::Return(None));
    m.add_function(f);

    assert!(Dce.run(&mut m), "DCE should report change");
    let f = &m.functions[0];
    assert_eq!(f.block(target).params.len(), 0, "dead block param must be removed");
    // The predecessor's branch arg list must shrink in lockstep.
    let entry_term = f.block(f.entry).terminator.as_ref().unwrap();
    match entry_term {
        Terminator::Branch(_, args) => assert_eq!(args.len(), 0,
            "predecessor branch arg must be dropped alongside the dead param"),
        _ => panic!(),
    }
    // The const(0) inst is now unreferenced and should also be DCE'd.
    let const_remains = f.blocks.iter()
        .flat_map(|b| b.insts.iter())
        .any(|i| matches!(i.kind, InstKind::ConstInt(0, _)));
    assert!(!const_remains, "const(0) should also be DCE'd after its only use disappears");
}

#[test]
fn audit_dce_keeps_live_block_param() {
    // A block param that IS used inside the block must NOT be removed.
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let target = f.create_block("target");
    let param_id = f.next_value_id();
    f.block_mut(target).params.push(BlockParam {
        id: param_id,
        ty: IrType::Int(IntWidth::I32),
    });
    let init = push(&mut f, InstKind::ConstInt(7, IntWidth::I32), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Branch(target, vec![init]));
    f.block_mut(target).terminator = Some(Terminator::Return(Some(param_id)));
    m.add_function(f);

    Dce.run(&mut m);
    let f = &m.functions[0];
    assert_eq!(f.block(target).params.len(), 1, "live block param must survive");
}

#[test]
fn audit_dce_removes_one_of_two_block_params_keeps_correct_arg() {
    // target(p0:i32, p1:i32): ret p1
    // entry: br target(c0, c1)
    // p0 is dead, p1 is live. Removing p0 must drop the matching
    // arg slot but keep p1 → c1.
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let target = f.create_block("target");
    let p0 = f.next_value_id();
    let p1 = f.next_value_id();
    f.block_mut(target).params.push(BlockParam { id: p0, ty: IrType::Int(IntWidth::I32) });
    f.block_mut(target).params.push(BlockParam { id: p1, ty: IrType::Int(IntWidth::I32) });
    let c0 = push(&mut f, InstKind::ConstInt(10, IntWidth::I32), IrType::Int(IntWidth::I32));
    let c1 = push(&mut f, InstKind::ConstInt(20, IntWidth::I32), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Branch(target, vec![c0, c1]));
    f.block_mut(target).terminator = Some(Terminator::Return(Some(p1)));
    m.add_function(f);

    Dce.run(&mut m);

    let f = &m.functions[0];
    let target_block = f.block(target);
    assert_eq!(target_block.params.len(), 1, "p0 should be removed");
    assert_eq!(target_block.params[0].id, p1, "p1 should remain at index 0");

    let entry_term = f.block(f.entry).terminator.as_ref().unwrap();
    match entry_term {
        Terminator::Branch(_, args) => {
            assert_eq!(args.len(), 1);
            assert_eq!(args[0], c1, "the surviving arg must be c1, not c0");
        }
        _ => panic!(),
    }

    // Now const(10) is dead — should also be gone after the
    // outer-loop re-runs the inner DCE.
    let c0_remains = f.blocks.iter()
        .flat_map(|b| b.insts.iter())
        .any(|i| i.id == c0);
    assert!(!c0_remains, "const(10) should be DCE'd after its arg slot is gone");
}

// =============================================================
// FINDING Med-5 / N-2 (cascading): DCE must iterate its outer
// loop until a fixpoint — removing one block param in round 1
// can expose a *different* block param as dead in round 2.
// =============================================================
//
// Shape (audit N-2 — the previous version of this test had both
// params dead from the start and could be satisfied in a single
// outer iteration, so it didn't actually pin cascading):
//
//   entry:
//     %c = const 5
//     br other(%c)              ; passes %c → p_other
//
//   other(p_other):
//     %u = ineg p_other         ; ONLY use of p_other
//     br target(%u)              ; passes %u → p_target
//
//   target(p_target):            ; p_target is dead
//     ret
//
// Required rounds:
//   R1: remove p_target, drop branch arg %u from other.
//   R1 inner DCE: %u is now dead, remove it.  Now p_other is
//                 unreferenced.
//   R2: remove p_other, drop branch arg %c from entry.
//   R2 inner DCE: %c is now dead, remove it.
//   R3: no more params to remove, exit.
//
// If the outer loop were stuck at 1 iteration, p_other would
// survive.
#[test]
fn audit_dce_cascading_across_outer_iterations() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Void);

    // entry: computes %c, branches to other
    let c = push(&mut f, InstKind::ConstInt(5, IntWidth::I32), IrType::Int(IntWidth::I32));

    // other: has param p_other, computes %u = ineg p_other, branches to target
    let other = f.create_block("other");
    let p_other = f.next_value_id();
    f.block_mut(other).params.push(BlockParam {
        id: p_other, ty: IrType::Int(IntWidth::I32),
    });
    let u = f.next_value_id();
    f.block_mut(other).insts.push(Inst {
        id: u, kind: InstKind::INeg(p_other),
        ty: IrType::Int(IntWidth::I32), span: dummy_span(),
    });

    // target: has param p_target, returns (p_target never used).
    let target = f.create_block("target");
    let p_target = f.next_value_id();
    f.block_mut(target).params.push(BlockParam {
        id: p_target, ty: IrType::Int(IntWidth::I32),
    });
    f.block_mut(target).terminator = Some(Terminator::Return(None));

    // Wire: entry → other(%c), other → target(%u)
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Branch(other, vec![c]));
    f.block_mut(other).terminator = Some(Terminator::Branch(target, vec![u]));
    m.add_function(f);

    Dce.run(&mut m);

    let f = &m.functions[0];

    // Both block params must be gone.
    assert_eq!(f.block(target).params.len(), 0,
        "p_target should be removed in round 1");
    assert_eq!(f.block(other).params.len(), 0,
        "p_other should be removed in round 2 (cascading)");

    // Entry's branch arg list must be empty.
    match f.block(f.entry).terminator.as_ref().unwrap() {
        Terminator::Branch(_, args) => assert_eq!(args.len(), 0,
            "entry's branch to other should have no args after cascade"),
        _ => panic!(),
    }
    // other's branch to target must also have no args.
    match f.block(other).terminator.as_ref().unwrap() {
        Terminator::Branch(_, args) => assert_eq!(args.len(), 0,
            "other's branch to target should have no args"),
        _ => panic!(),
    }

    // All formerly-live values should be DCE'd: %c, %u.
    let any_inst = f.blocks.iter()
        .flat_map(|b| b.insts.iter())
        .any(|i| i.id == c || i.id == u);
    assert!(!any_inst,
        "dead instructions %c (const) and %u (ineg) should be DCE'd after the cascade");
}

// =============================================================
// FINDING M-4 (nested loops): natural-loop discovery must not
// inflate an outer loop's body by following back-edges of an
// inner loop into paths that only reach the outer header
// through the inner loop.
// =============================================================
//
// CFG:
//   entry → outer_header → inner_header → inner_latch → inner_header
//                          ↑                                      ↓
//                          outer_latch ←───────────────────────── ↓
//                          ↓
//                          exit
//
// Two loops:
//   - Inner: header=inner_header, latch=inner_latch.
//     Body = {inner_header, inner_latch}.
//   - Outer: header=outer_header, latch=outer_latch.
//     Body = {outer_header, inner_header, inner_latch, outer_latch}.
//
// The concern is that when computing the outer loop's body by
// walking preds(outer_latch) backwards, the BFS must correctly
// walk THROUGH the inner loop body. It must NOT absorb blocks
// outside both loops, and it MUST stop at the outer header.
#[test]
fn audit_licm_nested_loop_body_computation() {
    use crate::opt::util::find_natural_loops;

    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Void);

    let outer_header = f.create_block("outer_header");
    let inner_header = f.create_block("inner_header");
    let inner_latch = f.create_block("inner_latch");
    let outer_latch = f.create_block("outer_latch");
    let exit = f.create_block("exit");
    let entry = f.entry;

    // entry → outer_header
    f.block_mut(entry).terminator = Some(Terminator::Branch(outer_header, vec![]));

    // outer_header: cond_br → inner_header / exit
    let c1 = f.next_value_id();
    f.block_mut(outer_header).insts.push(Inst {
        id: c1,
        kind: InstKind::ConstBool(true),
        ty: IrType::Bool,
        span: dummy_span(),
    });
    f.block_mut(outer_header).terminator = Some(Terminator::CondBranch {
        cond: c1,
        true_dest: inner_header,
        true_args: vec![],
        false_dest: exit,
        false_args: vec![],
    });

    // inner_header: cond_br → inner_latch / outer_latch
    let c2 = f.next_value_id();
    f.block_mut(inner_header).insts.push(Inst {
        id: c2,
        kind: InstKind::ConstBool(true),
        ty: IrType::Bool,
        span: dummy_span(),
    });
    f.block_mut(inner_header).terminator = Some(Terminator::CondBranch {
        cond: c2,
        true_dest: inner_latch,
        true_args: vec![],
        false_dest: outer_latch,
        false_args: vec![],
    });

    // inner_latch → inner_header (back-edge)
    f.block_mut(inner_latch).terminator = Some(Terminator::Branch(inner_header, vec![]));

    // outer_latch → outer_header (back-edge)
    f.block_mut(outer_latch).terminator = Some(Terminator::Branch(outer_header, vec![]));

    // exit: return
    f.block_mut(exit).terminator = Some(Terminator::Return(None));

    m.add_function(f);

    let loops = find_natural_loops(&m.functions[0]);
    assert_eq!(loops.len(), 2, "expected exactly two natural loops");

    // Find the inner and outer loops by their headers.
    let inner = loops.iter().find(|l| l.header == inner_header).expect("inner loop not found");
    let outer = loops.iter().find(|l| l.header == outer_header).expect("outer loop not found");

    // Inner loop body: {inner_header, inner_latch}.
    assert_eq!(inner.body.len(), 2,
        "inner body should be {{inner_header, inner_latch}}, got {:?}", inner.body);
    assert!(inner.body.contains(&inner_header));
    assert!(inner.body.contains(&inner_latch));

    // Outer loop body: {outer_header, inner_header, inner_latch, outer_latch}.
    // MUST NOT contain entry or exit.
    assert_eq!(outer.body.len(), 4,
        "outer body should be {{outer_header, inner_header, inner_latch, outer_latch}}, got {:?}",
        outer.body);
    assert!(outer.body.contains(&outer_header));
    assert!(outer.body.contains(&inner_header));
    assert!(outer.body.contains(&inner_latch));
    assert!(outer.body.contains(&outer_latch));
    assert!(!outer.body.contains(&entry), "outer body must not include entry");
    assert!(!outer.body.contains(&exit), "outer body must not include exit");
}

// =============================================================
// FINDING M-7 (multi-latch): a loop with both a self-loop on
// the header AND a separate distinct latch.
// =============================================================
//
// CFG:
//     entry → header
//     header → header (self-loop, true_dest = header itself)
//     header → other
//     other → header (back-edge from a separate latch)
//     header → exit (one of the cond branches)
//
// Both latches map to the same header. The body should include
// {header, other}, NOT entry. find_preheader should still pick
// entry as the preheader.
#[test]
fn audit_licm_multi_latch_with_self_loop() {
    use crate::opt::util::find_natural_loops;

    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Void);
    let header = f.create_block("header");
    let other = f.create_block("other");
    let exit = f.create_block("exit");
    let entry = f.entry;

    // entry → header
    f.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![]));
    // header → header (true) or other (false), with cond
    let cond = f.next_value_id();
    f.block_mut(header).insts.push(Inst {
        id: cond,
        kind: InstKind::ConstBool(true),
        ty: IrType::Bool,
        span: dummy_span(),
    });
    f.block_mut(header).terminator = Some(Terminator::CondBranch {
        cond,
        true_dest: header,
        true_args: vec![],
        false_dest: other,
        false_args: vec![],
    });
    // other → header (back edge) or exit
    let cond2 = f.next_value_id();
    f.block_mut(other).insts.push(Inst {
        id: cond2,
        kind: InstKind::ConstBool(false),
        ty: IrType::Bool,
        span: dummy_span(),
    });
    f.block_mut(other).terminator = Some(Terminator::CondBranch {
        cond: cond2,
        true_dest: header,
        true_args: vec![],
        false_dest: exit,
        false_args: vec![],
    });
    f.block_mut(exit).terminator = Some(Terminator::Return(None));
    m.add_function(f);

    let loops = find_natural_loops(&m.functions[0]);
    assert_eq!(loops.len(), 1, "expected exactly one natural loop");
    let lp = &loops[0];
    assert_eq!(lp.header, header);
    // Body must include header and other, NOT entry.
    assert!(lp.body.contains(&header), "body must include header");
    assert!(lp.body.contains(&other),  "body must include other");
    assert!(!lp.body.contains(&entry), "body must NOT include the preheader (entry)");
    assert!(!lp.body.contains(&exit),  "body must NOT include the exit block");
    // Latches: both header (self-loop) and other (back edge).
    assert_eq!(lp.latches.len(), 2, "expected two latches");
    assert!(lp.latches.contains(&header));
    assert!(lp.latches.contains(&other));
}

// =============================================================
// FINDING C-2: LICM is dormant when locals stay in alloca slots
// =============================================================
//
// Mirrors the loop_sum.f90 IR shape: a load inside the loop body
// blocks invariant analysis. LICM should NOT hoist the load (correct
// without alias analysis), but it also has nothing else to hoist.
// Document the constraint via test.
#[test]
fn audit_licm_dormant_with_alloca_load() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Void);
    let slot = push(&mut f,
        InstKind::Alloca(IrType::Int(IntWidth::I32)),
        IrType::Ptr(Box::new(IrType::Int(IntWidth::I32))));
    let init = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));
    push(&mut f, InstKind::Store(init, slot), IrType::Void);

    let header = f.create_block("header");
    let i_param = f.next_value_id();
    f.block_mut(header).params.push(BlockParam { id: i_param, ty: IrType::Int(IntWidth::I32) });
    let v = f.next_value_id();
    f.block_mut(header).insts.push(Inst {
        id: v,
        kind: InstKind::Load(slot),
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
    let next = f.next_value_id();
    f.block_mut(header).insts.push(Inst {
        id: next,
        kind: InstKind::IAdd(v, one),
        ty: IrType::Int(IntWidth::I32),
        span: dummy_span(),
    });
    let exit = f.create_block("exit");
    f.block_mut(exit).terminator = Some(Terminator::Return(None));
    f.block_mut(header).terminator = Some(Terminator::CondBranch {
        cond: i_param, // phony cond, just to have a back edge
        true_dest: header,
        true_args: vec![next],
        false_dest: exit,
        false_args: vec![],
    });
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![init]));
    m.add_function(f);

    // The const(1) inside the header IS loop-invariant — LICM should hoist it.
    Licm.run(&mut m);
    let entry_block = m.functions[0].block(m.functions[0].entry);
    let const1_in_entry = entry_block.insts.iter()
        .any(|i| matches!(i.kind, InstKind::ConstInt(1, IntWidth::I32)));
    assert!(const1_in_entry,
        "LICM should have hoisted const(1) into the preheader");
    // The Load should NOT have been hoisted.
    let header_block = &m.functions[0].blocks[1];
    let load_still_in_header = header_block.insts.iter()
        .any(|i| matches!(i.kind, InstKind::Load(_)));
    assert!(load_still_in_header,
        "LICM must not hoist a Load (no alias analysis)");
}

// =============================================================
// FINDING M-8: LICM must not corrupt the IR through PassManager
// =============================================================
//
// Run a synthetic loop module through the full O2 pipeline. Anything
// that fails the verifier was a real soundness bug.
#[test]
fn audit_pipeline_o2_e2e_loop_through_passmanager() {
    use crate::opt::{build_pipeline, OptLevel};

    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Void);
    // Build: entry { %0=const 0; br header(%0) }
    //        header(%i:i32) { %k=const 5; %i2=iadd %i, %k; %lim=const 10;
    //                         %d=icmp ge %i, %lim; cbr %d, exit, latch(%i2) }
    //        latch(%i_in:i32) { %1=const 1; %n=iadd %i_in, %1; br header(%n) }
    //        exit { ret }
    let init = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));

    let header = f.create_block("header");
    let i_param = f.next_value_id();
    f.block_mut(header).params.push(BlockParam { id: i_param, ty: IrType::Int(IntWidth::I32) });

    let k = f.next_value_id();
    f.block_mut(header).insts.push(Inst {
        id: k,
        kind: InstKind::ConstInt(5, IntWidth::I32),
        ty: IrType::Int(IntWidth::I32),
        span: dummy_span(),
    });
    let i2 = f.next_value_id();
    f.block_mut(header).insts.push(Inst {
        id: i2,
        kind: InstKind::IAdd(i_param, k),
        ty: IrType::Int(IntWidth::I32),
        span: dummy_span(),
    });
    let lim = f.next_value_id();
    f.block_mut(header).insts.push(Inst {
        id: lim,
        kind: InstKind::ConstInt(10, IntWidth::I32),
        ty: IrType::Int(IntWidth::I32),
        span: dummy_span(),
    });
    let done = f.next_value_id();
    f.block_mut(header).insts.push(Inst {
        id: done,
        kind: InstKind::ICmp(CmpOp::Ge, i_param, lim),
        ty: IrType::Bool,
        span: dummy_span(),
    });

    let latch = f.create_block("latch");
    let i_in = f.next_value_id();
    f.block_mut(latch).params.push(BlockParam { id: i_in, ty: IrType::Int(IntWidth::I32) });
    let one = f.next_value_id();
    f.block_mut(latch).insts.push(Inst {
        id: one,
        kind: InstKind::ConstInt(1, IntWidth::I32),
        ty: IrType::Int(IntWidth::I32),
        span: dummy_span(),
    });
    let next = f.next_value_id();
    f.block_mut(latch).insts.push(Inst {
        id: next,
        kind: InstKind::IAdd(i_in, one),
        ty: IrType::Int(IntWidth::I32),
        span: dummy_span(),
    });
    f.block_mut(latch).terminator = Some(Terminator::Branch(header, vec![next]));

    let exit = f.create_block("exit");
    f.block_mut(exit).terminator = Some(Terminator::Return(None));

    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![init]));
    f.block_mut(header).terminator = Some(Terminator::CondBranch {
        cond: done,
        true_dest: exit,
        true_args: vec![],
        false_dest: latch,
        false_args: vec![i2],
    });
    m.add_function(f);

    // verify the input is well-formed
    assert!(verify_module(&m).is_empty(), "test setup invalid");

    let pm = build_pipeline(OptLevel::O2);
    pm.run(&mut m);

    // Final IR must verify.
    let errs = verify_module(&m);
    assert!(errs.is_empty(), "O2 pipeline left an invalid module: {:?}", errs);
}

// =============================================================
// FINDING (interaction): const_prop drops a block whose ONLY use
// of a value is now gone — does DCE then drop the value too?
// =============================================================
//
// Pipeline: const_prop folds CondBranch and prunes the dead arm,
// which removes the only consumer of some constant. DCE should then
// remove the constant. Verify the cooperation works end-to-end.
#[test]
fn audit_interaction_const_prop_then_dce_removes_orphan_const() {
    use crate::opt::{build_pipeline, OptLevel};

    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Void);
    let cond = push(&mut f, InstKind::ConstBool(true), IrType::Bool);
    // This constant is ONLY used in the about-to-be-dead else block.
    let dead_only = push(&mut f, InstKind::ConstInt(99, IntWidth::I32), IrType::Int(IntWidth::I32));

    let then_b = f.create_block("then");
    let else_b = f.create_block("else");

    // Use dead_only inside else (will be dropped by const_prop).
    let unused = f.next_value_id();
    f.block_mut(else_b).insts.push(Inst {
        id: unused,
        kind: InstKind::INeg(dead_only),
        ty: IrType::Int(IntWidth::I32),
        span: dummy_span(),
    });
    f.block_mut(else_b).terminator = Some(Terminator::Return(None));
    f.block_mut(then_b).terminator = Some(Terminator::Return(None));

    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::CondBranch {
        cond,
        true_dest: then_b,
        true_args: vec![],
        false_dest: else_b,
        false_args: vec![],
    });
    m.add_function(f);

    let pm = build_pipeline(OptLevel::O2);
    pm.run(&mut m);

    let post = verify_module(&m);
    assert!(post.is_empty(), "post-pipeline IR invalid: {:?}", post);

    // After: else block gone, const(99) and ineg gone too.
    let f = &m.functions[0];
    assert!(!f.blocks.iter().any(|b| b.id == else_b), "else block should be pruned");
    let dead_remains = f.blocks.iter()
        .flat_map(|b| b.insts.iter())
        .any(|i| matches!(i.kind, InstKind::ConstInt(99, _)));
    assert!(!dead_remains, "const(99) should be DCE'd after const_prop drops its only use");
}

// =============================================================
// FINDING (interaction): strength_reduce + DCE remove the orphan
// constant created by an Identity rewrite.
// =============================================================
#[test]
fn audit_interaction_strength_reduce_orphans_get_dced() {
    use crate::opt::{build_pipeline, OptLevel};

    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let x = push(&mut f, InstKind::ConstInt(42, IntWidth::I32), IrType::Int(IntWidth::I32));
    let one = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
    let mul = push(&mut f, InstKind::IMul(x, one), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(mul)));
    m.add_function(f);

    let pm = build_pipeline(OptLevel::O2);
    pm.run(&mut m);

    // strength_reduce makes mul an identity (passes through to x), then
    // turns the original mul inst into Const(0). DCE should remove that.
    let post = verify_module(&m);
    assert!(post.is_empty(), "post-pipeline IR invalid: {:?}", post);

    let f = &m.functions[0];
    // The terminator should now reference x directly.
    let term_val = match f.blocks[0].terminator.as_ref().unwrap() {
        Terminator::Return(Some(v)) => *v,
        _ => panic!(),
    };
    assert_eq!(term_val, x, "expected return to be x directly");

    // The orphan placeholder should be gone.
    let extra_const = f.blocks[0].insts.iter()
        .filter(|i| matches!(i.kind, InstKind::ConstInt(0, _)))
        .count();
    assert_eq!(extra_const, 0,
        "strength_reduce orphan placeholder Const(0) should be DCE'd");
}

// =============================================================
// FINDING M-B: const_fold Shl with count == width must NOT
// fold to 0 (AArch64 LSL masks the count, not zeros it).
// =============================================================
#[test]
fn audit_const_fold_shl_at_width_bails() {
    // shl 1_i32, 32_i32 — count equals width, AArch64 masks to 0,
    // so the runtime answer is 1. The fold MUST NOT silently emit 0.
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let v = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
    let cnt = push(&mut f, InstKind::ConstInt(32, IntWidth::I32), IrType::Int(IntWidth::I32));
    let s = push(&mut f, InstKind::Shl(v, cnt), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(s)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == s).unwrap();
    // Acceptable outcomes: leave the Shl alone, OR mask the count.
    // NOT acceptable: ConstInt(0, _).
    if let InstKind::ConstInt(0, _) = folded.kind {
        panic!("audit M-B: shl with count==width folded to 0 — diverges from AArch64 LSL");
    }
}

// =============================================================
// FINDING M-C: const_fold Shl with negative count must not fold to 0
// =============================================================
#[test]
fn audit_const_fold_shl_negative_count_bails() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let v = push(&mut f, InstKind::ConstInt(7, IntWidth::I32), IrType::Int(IntWidth::I32));
    let cnt = push(&mut f, InstKind::ConstInt(-1, IntWidth::I32), IrType::Int(IntWidth::I32));
    let s = push(&mut f, InstKind::Shl(v, cnt), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(s)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == s).unwrap();
    if let InstKind::ConstInt(0, _) = folded.kind {
        panic!("audit M-C: shl with negative count folded to 0 — wrong on AArch64");
    }
}

// =============================================================
// FINDING M-C: same for LShr / AShr
// =============================================================
#[test]
fn audit_const_fold_lshr_negative_count_bails() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let v = push(&mut f, InstKind::ConstInt(64, IntWidth::I32), IrType::Int(IntWidth::I32));
    let cnt = push(&mut f, InstKind::ConstInt(-2, IntWidth::I32), IrType::Int(IntWidth::I32));
    let s = push(&mut f, InstKind::LShr(v, cnt), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(s)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == s).unwrap();
    if let InstKind::ConstInt(0, _) = folded.kind {
        panic!("audit M-C: lshr with negative count folded to 0");
    }
}

// =============================================================
// FINDING N-1 / N-9 (re-re-audit): width drift in integer
// arithmetic folds. The B-1 Select fix was applied only to
// Select; the same shape exists in every binary int op. The N-9
// root-cause fix normalizes at `Const::from_inst`, so every
// downstream fold now operates on canonical (sign-extended)
// values regardless of how the source ConstInt is stored.
// =============================================================
//
// Each test builds a synthetic i32 operation whose operands are
// ConstInt(_, I8) with raw values that represent different signed
// values. Without N-9, the folds would compute on the raw i64
// storage and produce wrong results. With N-9, the stored values
// are canonicalized at insert, so the fold gets the correct
// signed interpretation.

#[test]
fn audit_const_fold_width_drift_iadd() {
    // a: ConstInt(255, I8)  = -1 at i8
    // b: ConstInt(2,   I8)  = 2  at i8
    // declared IAdd type: i32
    // Expected: -1 + 2 = 1
    // Pre-fix: 255 + 2 = 257 (wrong)
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let a = f.next_value_id();
    f.block_mut(f.entry).insts.push(Inst {
        id: a, kind: InstKind::ConstInt(255, IntWidth::I8),
        ty: IrType::Int(IntWidth::I8), span: dummy_span(),
    });
    let b = f.next_value_id();
    f.block_mut(f.entry).insts.push(Inst {
        id: b, kind: InstKind::ConstInt(2, IntWidth::I8),
        ty: IrType::Int(IntWidth::I8), span: dummy_span(),
    });
    let sum = f.next_value_id();
    f.block_mut(f.entry).insts.push(Inst {
        id: sum, kind: InstKind::IAdd(a, b),
        ty: IrType::Int(IntWidth::I32), span: dummy_span(),
    });
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(sum)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == sum).unwrap();
    match folded.kind {
        InstKind::ConstInt(1, IntWidth::I32) => { /* good */ }
        InstKind::ConstInt(v, w) => panic!(
            "audit N-1 IAdd: expected ConstInt(1, I32), got ConstInt({}, {:?}) — width drift",
            v, w
        ),
        ref other => panic!("expected ConstInt fold, got {:?}", other),
    }
}

#[test]
fn audit_const_fold_width_drift_isub() {
    // a: ConstInt(250, I8)  = -6
    // b: ConstInt(1,   I8)  =  1
    // Expected: -6 - 1 = -7
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let a = f.next_value_id();
    f.block_mut(f.entry).insts.push(Inst {
        id: a, kind: InstKind::ConstInt(250, IntWidth::I8),
        ty: IrType::Int(IntWidth::I8), span: dummy_span(),
    });
    let b = f.next_value_id();
    f.block_mut(f.entry).insts.push(Inst {
        id: b, kind: InstKind::ConstInt(1, IntWidth::I8),
        ty: IrType::Int(IntWidth::I8), span: dummy_span(),
    });
    let diff = f.next_value_id();
    f.block_mut(f.entry).insts.push(Inst {
        id: diff, kind: InstKind::ISub(a, b),
        ty: IrType::Int(IntWidth::I32), span: dummy_span(),
    });
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(diff)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == diff).unwrap();
    match folded.kind {
        InstKind::ConstInt(-7, IntWidth::I32) => { /* good */ }
        ref other => panic!("audit N-1 ISub: expected ConstInt(-7, I32), got {:?}", other),
    }
}

#[test]
fn audit_const_fold_width_drift_imul() {
    // a: ConstInt(255, I8)  = -1
    // b: ConstInt(3,   I8)  =  3
    // Expected: -1 * 3 = -3
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let a = f.next_value_id();
    f.block_mut(f.entry).insts.push(Inst {
        id: a, kind: InstKind::ConstInt(255, IntWidth::I8),
        ty: IrType::Int(IntWidth::I8), span: dummy_span(),
    });
    let b = f.next_value_id();
    f.block_mut(f.entry).insts.push(Inst {
        id: b, kind: InstKind::ConstInt(3, IntWidth::I8),
        ty: IrType::Int(IntWidth::I8), span: dummy_span(),
    });
    let prod = f.next_value_id();
    f.block_mut(f.entry).insts.push(Inst {
        id: prod, kind: InstKind::IMul(a, b),
        ty: IrType::Int(IntWidth::I32), span: dummy_span(),
    });
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(prod)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == prod).unwrap();
    match folded.kind {
        InstKind::ConstInt(-3, IntWidth::I32) => { /* good */ }
        ref other => panic!("audit N-1 IMul: expected ConstInt(-3, I32), got {:?}", other),
    }
}

#[test]
fn audit_const_fold_width_drift_idiv() {
    // a: ConstInt(255, I8)  = -1
    // b: ConstInt(2,   I8)  =  2
    // Expected: -1 / 2 = 0 (toward zero)
    // Pre-fix: 255 / 2 = 127 (very wrong)
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let a = f.next_value_id();
    f.block_mut(f.entry).insts.push(Inst {
        id: a, kind: InstKind::ConstInt(255, IntWidth::I8),
        ty: IrType::Int(IntWidth::I8), span: dummy_span(),
    });
    let b = f.next_value_id();
    f.block_mut(f.entry).insts.push(Inst {
        id: b, kind: InstKind::ConstInt(2, IntWidth::I8),
        ty: IrType::Int(IntWidth::I8), span: dummy_span(),
    });
    let q = f.next_value_id();
    f.block_mut(f.entry).insts.push(Inst {
        id: q, kind: InstKind::IDiv(a, b),
        ty: IrType::Int(IntWidth::I32), span: dummy_span(),
    });
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(q)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == q).unwrap();
    match folded.kind {
        InstKind::ConstInt(0, IntWidth::I32) => { /* good */ }
        ref other => panic!("audit N-1 IDiv: expected ConstInt(0, I32), got {:?}", other),
    }
}

#[test]
fn audit_const_fold_width_drift_icmp() {
    // a: ConstInt(255, I8)  = -1
    // b: ConstInt(0,   I8)  =  0
    // ICmp Lt: -1 < 0 → true
    // Pre-fix (if ICmp also discarded widths): 255 < 0 → false
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Bool);
    let a = f.next_value_id();
    f.block_mut(f.entry).insts.push(Inst {
        id: a, kind: InstKind::ConstInt(255, IntWidth::I8),
        ty: IrType::Int(IntWidth::I8), span: dummy_span(),
    });
    let b = f.next_value_id();
    f.block_mut(f.entry).insts.push(Inst {
        id: b, kind: InstKind::ConstInt(0, IntWidth::I8),
        ty: IrType::Int(IntWidth::I8), span: dummy_span(),
    });
    let lt = f.next_value_id();
    f.block_mut(f.entry).insts.push(Inst {
        id: lt, kind: InstKind::ICmp(CmpOp::Lt, a, b),
        ty: IrType::Bool, span: dummy_span(),
    });
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(lt)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == lt).unwrap();
    match folded.kind {
        InstKind::ConstBool(true) => { /* good */ }
        ref other => panic!("audit N-1 ICmp: expected ConstBool(true), got {:?}", other),
    }
}

// =============================================================
// FINDING B-8: CSE must dedupe semantically-equal ConstInts
// stored at different bit patterns at the same width.
// =============================================================
//
// `ConstInt(255, I8)` and `ConstInt(-1, I8)` both encode -1 at i8
// precision. After the B-8 fix, CSE keys on the sign-extended
// value, so the two should collapse to a single SSA name.
#[test]
fn audit_cse_dedupes_const_int_bit_pattern() {
    use crate::opt::LocalCse;
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I8));
    let a = push(&mut f, InstKind::ConstInt(255, IntWidth::I8), IrType::Int(IntWidth::I8));
    let b = push(&mut f, InstKind::ConstInt(-1,  IntWidth::I8), IrType::Int(IntWidth::I8));
    // Use a in some op and b in another so neither is dead.
    let neg_a = push(&mut f, InstKind::INeg(a), IrType::Int(IntWidth::I8));
    let neg_b = push(&mut f, InstKind::INeg(b), IrType::Int(IntWidth::I8));
    // Add them so both are live to the terminator.
    let sum = push(&mut f, InstKind::IAdd(neg_a, neg_b), IrType::Int(IntWidth::I8));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(sum)));
    m.add_function(f);

    LocalCse.run(&mut m);

    // After CSE, one of {a, b} has been rewritten to point at the
    // other. The two INegs should now reference the same value.
    let f = &m.functions[0];
    let neg_a_inst = f.blocks[0].insts.iter().find(|i| i.id == neg_a).unwrap();
    let neg_b_inst = f.blocks[0].insts.iter().find(|i| i.id == neg_b).unwrap();
    let neg_a_op = match &neg_a_inst.kind { InstKind::INeg(v) => *v, _ => panic!() };
    let neg_b_op = match &neg_b_inst.kind { InstKind::INeg(v) => *v, _ => panic!() };
    assert_eq!(neg_a_op, neg_b_op,
        "audit B-8: ConstInt(255, I8) and ConstInt(-1, I8) should CSE-dedupe \
         (both represent -1 at i8 precision), but the INeg operands differ");
}

// =============================================================
// FINDING M-C: same for AShr (was missing from first round)
// =============================================================
#[test]
fn audit_const_fold_ashr_negative_count_bails() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let v = push(&mut f, InstKind::ConstInt(-128, IntWidth::I32), IrType::Int(IntWidth::I32));
    let cnt = push(&mut f, InstKind::ConstInt(-3, IntWidth::I32), IrType::Int(IntWidth::I32));
    let s = push(&mut f, InstKind::AShr(v, cnt), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(s)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == s).unwrap();
    if let InstKind::ConstInt(0, _) = folded.kind {
        panic!("audit M-C: ashr with negative count folded to 0 — wrong on AArch64");
    }
}

// =============================================================
// FINDING Med-6: LICM must not hoist trap-prone IDiv
// =============================================================
#[test]
fn audit_licm_does_not_hoist_idiv() {
    // Build a loop with an invariant `idiv a, b` in the body.
    // Without the fix, LICM would happily hoist it to the preheader,
    // potentially executing it when the original loop would have
    // skipped it (causing SIGFPE on b == 0).
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Void);
    let a = push(&mut f, InstKind::ConstInt(100, IntWidth::I32), IrType::Int(IntWidth::I32));
    let b = push(&mut f, InstKind::ConstInt(5,   IntWidth::I32), IrType::Int(IntWidth::I32));
    let init = push(&mut f, InstKind::ConstInt(0, IntWidth::I32), IrType::Int(IntWidth::I32));

    let header = f.create_block("header");
    let i_param = f.next_value_id();
    f.block_mut(header).params.push(BlockParam { id: i_param, ty: IrType::Int(IntWidth::I32) });

    // Inside the body: %q = idiv a, b — both operands invariant.
    let q = f.next_value_id();
    f.block_mut(header).insts.push(Inst {
        id: q,
        kind: InstKind::IDiv(a, b),
        ty: IrType::Int(IntWidth::I32),
        span: dummy_span(),
    });
    // %sum = iadd i_param, q — depends on q, not invariant.
    let sum = f.next_value_id();
    f.block_mut(header).insts.push(Inst {
        id: sum,
        kind: InstKind::IAdd(i_param, q),
        ty: IrType::Int(IntWidth::I32),
        span: dummy_span(),
    });
    let limit = f.next_value_id();
    f.block_mut(header).insts.push(Inst {
        id: limit,
        kind: InstKind::ConstInt(10, IntWidth::I32),
        ty: IrType::Int(IntWidth::I32),
        span: dummy_span(),
    });
    let done = f.next_value_id();
    f.block_mut(header).insts.push(Inst {
        id: done,
        kind: InstKind::ICmp(CmpOp::Ge, sum, limit),
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
        false_args: vec![sum],
    });
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Branch(header, vec![init]));
    m.add_function(f);

    Licm.run(&mut m);
    let f = &m.functions[0];
    // The IDiv must STILL be in the header block, not in entry.
    let header_block = f.blocks.iter().find(|b| b.name.starts_with("header")).unwrap();
    let entry_block = f.block(f.entry);
    let idiv_in_header = header_block.insts.iter().any(|i| matches!(i.kind, InstKind::IDiv(..)));
    let idiv_in_entry  = entry_block.insts.iter().any(|i| matches!(i.kind, InstKind::IDiv(..)));
    assert!(idiv_in_header, "audit Med-6: LICM should leave IDiv in body (trap-prone)");
    assert!(!idiv_in_entry, "audit Med-6: LICM must not hoist IDiv into preheader");
}

// =============================================================
// FINDING C-C: const_fold Select must use the Select's declared
// type, not the chosen branch's source width.
// =============================================================
//
// Happy-path test: same widths everywhere. Pinned for regression
// against the simple form of the bug.
#[test]
fn audit_const_fold_select_uses_declared_type() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let a = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
    let b = push(&mut f, InstKind::ConstInt(99, IntWidth::I32), IrType::Int(IntWidth::I32));
    let cond = push(&mut f, InstKind::ConstBool(true), IrType::Bool);
    let sel = push(&mut f, InstKind::Select(cond, a, b), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(sel)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == sel).unwrap();
    match folded.kind {
        InstKind::ConstInt(1, IntWidth::I32) => { /* good */ }
        ref other => panic!("expected ConstInt(1, I32), got {:?}", other),
    }
    assert_eq!(folded.ty, IrType::Int(IntWidth::I32));
}

// =============================================================
// FINDING B-1 (re-audit): C-C fix is incomplete for width-drift
// within `IrType::Int(_)`. Pin the bug with an adversarial test
// that would have failed BEFORE the B-1 fix.
// =============================================================
//
// Build a Select declared `i64` whose chosen branch is a
// `ConstInt(255, I8)` — that's `-1` at i8 precision. Without
// source-width sign extension, the fold produces `ConstInt(255,
// I64)` instead of `ConstInt(-1, I64)`.
//
// We construct the IR by manually inserting a ConstInt with a
// declared `inst.ty` of `Int(I8)` AND a Select whose `inst.ty` is
// `Int(I64)`. The verifier wouldn't normally allow this (operand
// types should match the Select's type), but the verifier only
// checks `is_int()`, not bit width — exactly the gap C-C was
// trying to defend against.
#[test]
fn audit_const_fold_select_width_drift_int() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I64));

    // Manually emit a ConstInt(255, I8). The stored value is 255.
    // At i8 precision the "true" value is -1 (sign bit set).
    let a = f.next_value_id();
    let entry = f.entry;
    f.block_mut(entry).insts.push(Inst {
        id: a,
        kind: InstKind::ConstInt(255, IntWidth::I8),
        ty: IrType::Int(IntWidth::I8),
        span: dummy_span(),
    });
    let b = push(&mut f, InstKind::ConstInt(99, IntWidth::I64), IrType::Int(IntWidth::I64));
    let cond = push(&mut f, InstKind::ConstBool(true), IrType::Bool);

    // Select declared as i64, chosen branch is i8 (255 → -1).
    let sel = f.next_value_id();
    f.block_mut(entry).insts.push(Inst {
        id: sel,
        kind: InstKind::Select(cond, a, b),
        ty: IrType::Int(IntWidth::I64),
        span: dummy_span(),
    });
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(sel)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == sel).unwrap();
    match folded.kind {
        InstKind::ConstInt(-1, IntWidth::I64) => { /* good — i8 -1 sign-extended to i64 */ }
        InstKind::ConstInt(v, w) => panic!(
            "audit B-1: expected ConstInt(-1, I64), got ConstInt({}, {:?}). \
             Source width was discarded — width drift slipped through.",
            v, w
        ),
        ref other => panic!("expected ConstInt fold, got {:?}", other),
    }
}

// =============================================================
// FINDING C-A (structural pin): strength_reduce must NOT replace
// the orphan instruction's kind with a placeholder Const(0).
// =============================================================
//
// The original C-A bug was inside a `_ => continue` branch that
// silently dropped `changed = true` after writing a Const(0)
// placeholder for non-int/non-bool result types. The structural
// fix removes the placeholder machinery entirely. Pin the fix by
// asserting the orphan instruction's kind is *unchanged* after a
// run of strength_reduce, then DCE removes it cleanly.
//
// Before the structural fix, the orphan would have its kind
// rewritten to `ConstInt(0, _)`. After the fix, it stays as
// `IMul(_, _)` until DCE removes it.
#[test]
fn audit_strength_reduce_identity_does_not_write_placeholder() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let x = push(&mut f, InstKind::ConstInt(42, IntWidth::I32), IrType::Int(IntWidth::I32));
    let one = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
    let mul = push(&mut f, InstKind::IMul(x, one), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(mul)));
    m.add_function(f);

    StrengthReduce.run(&mut m);
    // The orphan instruction at `mul`'s ID must still exist and
    // have its original kind. The earlier (broken) version would
    // have replaced its kind with ConstInt(0, _).
    let orphan = m.functions[0].blocks[0].insts.iter().find(|i| i.id == mul).unwrap();
    match orphan.kind {
        InstKind::IMul(_, _) => { /* good — original kind preserved */ }
        InstKind::ConstInt(0, _) => panic!(
            "audit C-A: orphan was rewritten to placeholder Const(0). \
             The structural fix should leave the original kind alone."
        ),
        ref other => panic!("unexpected orphan kind: {:?}", other),
    }
}

// =============================================================
// FINDING C-A: strength_reduce Identity rewrites must always set
// `changed = true`, even if the placeholder branch is taken.
// (After the fix, no placeholder is written at all.)
// =============================================================
#[test]
fn audit_strength_reduce_identity_reports_changed() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let x = push(&mut f, InstKind::ConstInt(42, IntWidth::I32), IrType::Int(IntWidth::I32));
    let one = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
    let mul = push(&mut f, InstKind::IMul(x, one), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(mul)));
    m.add_function(f);

    let changed = StrengthReduce.run(&mut m);
    assert!(changed, "strength_reduce must report `changed = true` after Identity rewrite");

    // The terminator now references x.
    match m.functions[0].blocks[0].terminator.as_ref().unwrap() {
        Terminator::Return(Some(v)) => assert_eq!(*v, x),
        _ => panic!(),
    }
}

// =============================================================
// FINDING M-D: ICmp must sign-extend each operand at its OWN width
// =============================================================
//
// Construct a synthetic ICmp where the two ConstInt operands have
// different declared widths and the same low-bits-as-stored value.
// Per M-D, before the fix the fold uses operand a's width for both,
// silently producing a wrong answer when widths differ.
#[test]
fn audit_const_fold_icmp_uses_each_operand_width() {
    // We can't easily produce mismatched widths through normal
    // lowering because the verifier would catch operand types in
    // most cases. Construct directly: a is i32 holding 255, b is i8
    // holding 255 (which represents -1 at i8 precision). Eq should
    // be FALSE: 255 != -1.
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Bool);
    let a = push(&mut f, InstKind::ConstInt(255, IntWidth::I32), IrType::Int(IntWidth::I32));
    let b = push(&mut f, InstKind::ConstInt(255, IntWidth::I8),  IrType::Int(IntWidth::I8));
    let eq = push(&mut f, InstKind::ICmp(CmpOp::Eq, a, b), IrType::Bool);
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(eq)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == eq).unwrap();
    match folded.kind {
        // After the M-D fix: bv sign-extended at i8 → -1; av at i32 → 255.
        // 255 == -1 → false.
        InstKind::ConstBool(false) => { /* good */ }
        // Before the fix: both sign-extended at i32 → 255 == 255 → true.
        InstKind::ConstBool(true) => panic!(
            "audit M-D: ICmp folded as if both operands were i32-width \
             (lost b's i8 width). Expected ConstBool(false)."
        ),
        ref other => panic!("expected ConstBool, got {:?}", other),
    }
}

// =============================================================
// FINDING M-E (auto-fixed by M-1): FCmp on f32 ConstFloats agrees
// with runtime f32 precision
// =============================================================
#[test]
fn audit_const_fold_fcmp_f32_after_m1_fix() {
    // After M-1, IntToFloat(16777217:i32, F32) folds to ConstFloat
    // with the f32-rounded value (16777216.0). FCmp Eq against
    // ConstFloat(16777216.0, F32) should now return true.
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Bool);
    let i = push(&mut f, InstKind::ConstInt(16_777_217, IntWidth::I32), IrType::Int(IntWidth::I32));
    let fv_a = push(&mut f, InstKind::IntToFloat(i, FloatWidth::F32), IrType::Float(FloatWidth::F32));
    let fv_b = push(&mut f, InstKind::ConstFloat(16_777_216.0, FloatWidth::F32), IrType::Float(FloatWidth::F32));
    let eq = push(&mut f, InstKind::FCmp(CmpOp::Eq, fv_a, fv_b), IrType::Bool);
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(eq)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == eq).unwrap();
    assert!(matches!(folded.kind, InstKind::ConstBool(true)),
        "audit M-E: FCmp on f32-rounded values should return true, got {:?}", folded.kind);
}

// =============================================================
// FINDING M-F (auto-fixed by M-1): FloatToInt of an f32 ConstFloat
// rounds correctly through f32
// =============================================================
#[test]
fn audit_const_fold_floattoint_from_f32_after_m1_fix() {
    // IntToFloat(16777217, F32) → ConstFloat(16777216.0, F32) after M-1.
    // FloatToInt(_, I32) should produce 16777216, not 16777217.
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let i = push(&mut f, InstKind::ConstInt(16_777_217, IntWidth::I32), IrType::Int(IntWidth::I32));
    let fv = push(&mut f, InstKind::IntToFloat(i, FloatWidth::F32), IrType::Float(FloatWidth::F32));
    let back = push(&mut f, InstKind::FloatToInt(fv, IntWidth::I32), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(back)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == back).unwrap();
    match folded.kind {
        InstKind::ConstInt(16_777_216, IntWidth::I32) => { /* good */ }
        ref other => panic!("audit M-F: expected ConstInt(16777216, I32), got {:?}", other),
    }
}

// =============================================================
// FINDING Med-3: PopCount/CLZ/CTZ should respect inst.ty for output
// =============================================================
#[test]
fn audit_const_fold_popcount_uses_inst_ty() {
    // Build PopCount whose source is i32 but whose declared result
    // type is also i32 (the common case). After the fix, the fold
    // result must be tagged with inst.ty (i32), even if a future
    // change makes the source carry a different width.
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let v = push(&mut f, InstKind::ConstInt(0xFF, IntWidth::I32), IrType::Int(IntWidth::I32));
    let pc = push(&mut f, InstKind::PopCount(v), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(pc)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == pc).unwrap();
    match folded.kind {
        InstKind::ConstInt(8, IntWidth::I32) => { /* good */ }
        ref other => panic!("expected ConstInt(8, I32), got {:?}", other),
    }
    assert_eq!(folded.ty, IrType::Int(IntWidth::I32));
}

// =============================================================
// FINDING M-1 (latent): IntToFloat→FSub gives the wrong answer
// =============================================================
//
// Demonstrates the downstream impact of M-1: chaining IntToFloat
// (f32) into FSub produces a non-zero result for two values that
// should be equal after f32 rounding. Once mem2reg lands and this
// IR pattern starts appearing from real Fortran (subtracting two
// `real(N, 4)` expressions), the bug becomes a CRITICAL miscompile.
#[test]
fn audit_int_to_f32_then_fsub_wrong_answer_today() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Float(FloatWidth::F32));
    let i_a = push(&mut f, InstKind::ConstInt(16_777_217, IntWidth::I32), IrType::Int(IntWidth::I32));
    let i_b = push(&mut f, InstKind::ConstInt(16_777_216, IntWidth::I32), IrType::Int(IntWidth::I32));
    let f_a = push(&mut f, InstKind::IntToFloat(i_a, FloatWidth::F32), IrType::Float(FloatWidth::F32));
    let f_b = push(&mut f, InstKind::IntToFloat(i_b, FloatWidth::F32), IrType::Float(FloatWidth::F32));
    let diff = push(&mut f, InstKind::FSub(f_a, f_b), IrType::Float(FloatWidth::F32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(diff)));
    m.add_function(f);

    ConstFold.run(&mut m);

    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == diff).unwrap();
    match folded.kind {
        // Expected: 0.0 (after correct f32 rounding both inputs are 16777216).
        // Bug:      1.0 (without rounding 16777217.0 - 16777216.0 = 1.0).
        InstKind::ConstFloat(v, FloatWidth::F32) => assert_eq!(v, 0.0,
            "expected 0.0 (both inputs round to 16777216 in f32), got {}", v),
        ref other => panic!("expected ConstFloat, got {:?}", other),
    }
}

// =============================================================
// FINDING M-11: const_fold imul i8 overflow wraps correctly
// =============================================================
#[test]
fn audit_const_fold_imul_i8_overflow_wraps() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I8));
    let a = push(&mut f, InstKind::ConstInt(100, IntWidth::I8), IrType::Int(IntWidth::I8));
    let b = push(&mut f, InstKind::ConstInt(100, IntWidth::I8), IrType::Int(IntWidth::I8));
    let p = push(&mut f, InstKind::IMul(a, b), IrType::Int(IntWidth::I8));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(p)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == p).unwrap();
    match folded.kind {
        // 100 * 100 = 10000; low byte = 0x10 = 16; sext = 16
        InstKind::ConstInt(v, IntWidth::I8) => assert_eq!(v, 16, "expected i8 wrap, got {}", v),
        _ => panic!(),
    }
}

// =============================================================
// FINDING N-8 (re-audit strengthened): const_fold must converge
// on non-RPO block orders too, via the pass-manager fixpoint.
// =============================================================
//
// `const_fold` walks `func.blocks` in vector order, not in
// reverse-postorder. If a fold in an early-vec block needs a
// constant defined by a later-vec block (that still dominates it
// in the CFG), one pass misses the fold. The pass-manager
// fixpoint re-runs const_fold until it converges.
//
// Audit N-8: the previous version of this test explicitly said
// "we don't fail if the fold didn't happen" — it only tested
// that the IR was still valid, not that the fold actually
// converged. This version asserts the sum is folded to `3` so a
// regression that stops the fixpoint from rescuing us would fail.
#[test]
fn audit_const_fold_non_rpo_block_order() {
    use crate::opt::{build_pipeline, OptLevel};

    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));

    let a_block = f.create_block("a");
    let b_block = f.create_block("b");
    let one = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Branch(a_block, vec![]));

    let two_id = f.next_value_id();
    f.block_mut(a_block).insts.push(Inst {
        id: two_id,
        kind: InstKind::ConstInt(2, IntWidth::I32),
        ty: IrType::Int(IntWidth::I32),
        span: dummy_span(),
    });
    f.block_mut(a_block).terminator = Some(Terminator::Branch(b_block, vec![]));

    let sum_id = f.next_value_id();
    f.block_mut(b_block).insts.push(Inst {
        id: sum_id,
        kind: InstKind::IAdd(one, two_id),
        ty: IrType::Int(IntWidth::I32),
        span: dummy_span(),
    });
    f.block_mut(b_block).terminator = Some(Terminator::Return(Some(sum_id)));

    // Swap A and B in the vec order (positions 1 and 2; entry is 0).
    f.blocks.swap(1, 2);
    m.add_function(f);

    let pre = verify_module(&m);
    assert!(pre.is_empty(), "test setup invalid: {:?}", pre);

    let pm = build_pipeline(OptLevel::O2);
    pm.run(&mut m);

    let post = verify_module(&m);
    assert!(post.is_empty(), "non-RPO block order broke optimization: {:?}", post);

    // The fold MUST have converged: the IAdd's defining instruction
    // (still carrying `sum_id`) should now be the constant `3`, or
    // the entire dead chain should have been DCE'd and the terminator
    // should reference a const(3) directly.
    let f = &m.functions[0];
    let terminator_val = f.blocks.iter()
        .find_map(|b| match &b.terminator {
            Some(Terminator::Return(Some(v))) => Some(*v),
            _ => None,
        })
        .expect("no Return terminator");
    let term_kind = f.blocks.iter()
        .flat_map(|b| b.insts.iter())
        .find(|i| i.id == terminator_val)
        .map(|i| i.kind.clone())
        .expect("terminator value has no defining inst");
    match term_kind {
        InstKind::ConstInt(3, IntWidth::I32) => { /* good — fold converged */ }
        ref other => panic!(
            "audit N-8: the non-RPO block order broke const_fold convergence. \
             Expected the return value to be ConstInt(3, I32), got {:?}",
            other
        ),
    }
}

// =============================================================
// FINDING M-9: const_fold large negative i64 left shift
// =============================================================
//
// Verify shl wrapping at width boundaries.
#[test]
fn audit_const_fold_shl_full_width_wrap() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));
    let v = push(&mut f, InstKind::ConstInt(1, IntWidth::I32), IrType::Int(IntWidth::I32));
    let cnt = push(&mut f, InstKind::ConstInt(31, IntWidth::I32), IrType::Int(IntWidth::I32));
    let s = push(&mut f, InstKind::Shl(v, cnt), IrType::Int(IntWidth::I32));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(s)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == s).unwrap();
    match folded.kind {
        InstKind::ConstInt(v, IntWidth::I32) => {
            // 1 << 31 in i32 is INT_MIN = -2147483648.
            assert_eq!(v, -2147483648, "expected i32 sign bit set, got {}", v);
        }
        _ => panic!(),
    }
}

// =============================================================
// FINDING M-10: const_fold ConstInt narrow signed overflow on iadd
// =============================================================
#[test]
fn audit_const_fold_iadd_i16_overflow() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I16));
    let a = push(&mut f, InstKind::ConstInt(32000, IntWidth::I16), IrType::Int(IntWidth::I16));
    let b = push(&mut f, InstKind::ConstInt(1000,  IntWidth::I16), IrType::Int(IntWidth::I16));
    let s = push(&mut f, InstKind::IAdd(a, b), IrType::Int(IntWidth::I16));
    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::Return(Some(s)));
    m.add_function(f);

    ConstFold.run(&mut m);
    let folded = m.functions[0].blocks[0].insts.iter().find(|i| i.id == s).unwrap();
    match folded.kind {
        InstKind::ConstInt(v, IntWidth::I16) => {
            // 32000 + 1000 = 33000, wraps in i16 to -32536
            assert_eq!(v, -32536, "expected i16 wrap, got {}", v);
        }
        _ => panic!(),
    }
}

// =============================================================
// FINDING M-6: const_prop must not break dominance when blocks branch
// to a merge that uses values from a now-dropped path.
// =============================================================
//
// Manually construct: entry → A or B; both → merge with a phi-style
// param. Fold the entry conditional to const(true) → drops B. Merge
// loses one predecessor; verifier must still accept the result.
#[test]
fn audit_const_prop_merge_after_drop() {
    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));

    let cond = push(&mut f, InstKind::ConstBool(true), IrType::Bool);
    let v_a = push(&mut f, InstKind::ConstInt(10, IntWidth::I32), IrType::Int(IntWidth::I32));
    let v_b = push(&mut f, InstKind::ConstInt(20, IntWidth::I32), IrType::Int(IntWidth::I32));

    let a = f.create_block("a");
    let b = f.create_block("b");
    let merge = f.create_block("merge");
    let m_param = f.next_value_id();
    f.block_mut(merge).params.push(BlockParam { id: m_param, ty: IrType::Int(IntWidth::I32) });

    f.block_mut(a).terminator = Some(Terminator::Branch(merge, vec![v_a]));
    f.block_mut(b).terminator = Some(Terminator::Branch(merge, vec![v_b]));
    f.block_mut(merge).terminator = Some(Terminator::Return(Some(m_param)));

    let entry = f.entry;
    f.block_mut(entry).terminator = Some(Terminator::CondBranch {
        cond,
        true_dest: a,
        true_args: vec![],
        false_dest: b,
        false_args: vec![],
    });
    m.add_function(f);

    let pre = verify_module(&m);
    assert!(pre.is_empty(), "test setup not valid: {:?}", pre);

    ConstProp.run(&mut m);

    let post = verify_module(&m);
    assert!(post.is_empty(),
        "const_prop produced an invalid module after dropping the false arm: {:?}", post);

    // After folding, B should be gone but merge should still exist.
    let f = &m.functions[0];
    assert!(!f.blocks.iter().any(|bk| bk.id == b), "block B should be pruned");
    assert!(f.blocks.iter().any(|bk| bk.id == merge), "merge should remain");
}
