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
#[test]
fn audit_const_fold_select_uses_declared_type() {
    // Build: %0 = const_int 1 : i32
    //        %1 = const_int 99 : i32
    //        %2 = const_bool true
    //        %3 = select %2, %0, %1 : i32     ← declared i32
    // After fold, %3 should be ConstInt(1, I32) — same width.
    // The bug pattern (chosen branch with mismatched width) is hard
    // to construct without violating verifier invariants, but we
    // can at least pin the "destination width drives output" rule.
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
    // The instruction's ty should still be I32.
    assert_eq!(folded.ty, IrType::Int(IntWidth::I32));
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
// FINDING: Const fold visit order across non-RPO blocks
// =============================================================
//
// When func.blocks vec is not in reverse-postorder, const_fold may
// miss folds in a single pass. Pass manager fixpoint covers this.
// Just verify it doesn't crash or produce invalid IR.
#[test]
fn audit_const_fold_non_rpo_block_order() {
    use crate::opt::{build_pipeline, OptLevel};

    let mut m = Module::new("t".into());
    let mut f = Function::new("f".into(), vec![], IrType::Int(IntWidth::I32));

    // Build entry → A → B with B's first use depending on A's const.
    // Then deliberately swap A and B in func.blocks so B comes before A
    // in vec order (but A still dominates B in the CFG).
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

    // Should still produce valid IR through the full pipeline (and the
    // fixpoint should converge to fold sum_id to const(3)).
    let pre = verify_module(&m);
    assert!(pre.is_empty(), "test setup invalid: {:?}", pre);

    let pm = build_pipeline(OptLevel::O2);
    pm.run(&mut m);

    let post = verify_module(&m);
    assert!(post.is_empty(), "non-RPO block order broke optimization: {:?}", post);

    // Verify the fold actually happened — sum_id should now be const(3).
    let f = &m.functions[0];
    let sum_kind = f.blocks.iter()
        .flat_map(|b| b.insts.iter())
        .find(|i| i.id == sum_id)
        .map(|i| i.kind.clone());
    if let Some(InstKind::ConstInt(v, IntWidth::I32)) = sum_kind {
        assert_eq!(v, 3, "expected sum folded to const(3), got {}", v);
    }
    // (We don't fail the test if the fold didn't happen — just verify
    // the IR is still valid.)
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
