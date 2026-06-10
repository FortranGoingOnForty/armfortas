use std::collections::BTreeSet;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from("tests/fixtures").join(name);
    assert!(path.exists(), "missing test fixture {}", path.display());
    path
}

fn capture_text(request: CaptureRequest, stage: Stage) -> String {
    let result = capture_from_path(&request).expect("capture should succeed");
    match result.get(stage) {
        Some(CapturedStage::Text(text)) => text.clone(),
        Some(CapturedStage::Run(_)) => panic!("expected text stage for {}", stage.as_str()),
        None => panic!("missing requested stage {}", stage.as_str()),
    }
}

#[test]
fn simple_local_i128_roundtrip_emits_wide_pair_moves_at_o0() {
    let asm = capture_text(
        CaptureRequest {
            input: fixture("integer16_local_roundtrip.f90"),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O0,
        },
        Stage::Asm,
    );

    assert!(
        asm.contains("movz x16, #42"),
        "backend should materialize the low 64-bit half of the i128 constant:\n{}",
        asm
    );
    assert!(
        asm.contains("mov x17, xzr"),
        "backend should zero the high 64-bit half of the i128 constant:\n{}",
        asm
    );
    assert!(
        asm.contains("stp x16, x17"),
        "backend should store i128 values as paired 64-bit writes:\n{}",
        asm
    );
    assert!(
        asm.contains("ldp x16, x17"),
        "backend should reload i128 values as paired 64-bit reads:\n{}",
        asm
    );
}

#[test]
fn simple_local_i128_add_emits_carry_chain_ops_at_o0() {
    let asm = capture_text(
        CaptureRequest {
            input: fixture("integer16_add.f90"),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O0,
        },
        Stage::Asm,
    );

    assert!(
        asm.contains("adds x16, x16, x8"),
        "backend should use ADDS for the low i128 word:\n{}",
        asm
    );
    assert!(
        asm.contains("adc x17, x17, x8"),
        "backend should use ADC for the high i128 word:\n{}",
        asm
    );
}

#[test]
fn simple_local_i128_subneg_emits_borrow_chain_ops_at_o0() {
    let asm = capture_text(
        CaptureRequest {
            input: fixture("integer16_subneg.f90"),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O0,
        },
        Stage::Asm,
    );

    assert!(
        asm.contains("subs x16, x16, x8"),
        "backend should use SUBS for the low i128 subtraction word:\n{}",
        asm
    );
    assert!(
        asm.contains("sbc x17, x17, x8"),
        "backend should use SBC for the high i128 subtraction word:\n{}",
        asm
    );
    assert!(
        asm.contains("subs x16, xzr, x16"),
        "backend should use SUBS from xzr for i128 negation:\n{}",
        asm
    );
}

#[test]
fn simple_local_i128_eqne_emits_pair_compare_ops_at_o0() {
    let asm = capture_text(
        CaptureRequest {
            input: fixture("integer16_eqne.f90"),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O0,
        },
        Stage::Asm,
    );

    assert!(
        asm.contains("cmp x16, x8"),
        "backend should compare the low i128 limb:\n{}",
        asm
    );
    assert!(
        asm.contains("cmp x17, x9"),
        "backend should compare the high i128 limb:\n{}",
        asm
    );
    assert!(
        asm.contains("cset w10, eq"),
        "backend should materialize low/high equality flags:\n{}",
        asm
    );
    assert!(
        asm.contains("and w10, w10, w11"),
        "backend should combine equality limbs with AND:\n{}",
        asm
    );
    assert!(
        asm.contains("cset w10, ne"),
        "backend should materialize low/high inequality flags:\n{}",
        asm
    );
    assert!(
        asm.contains("orr w10, w10, w11"),
        "backend should combine inequality limbs with OR:\n{}",
        asm
    );
}

#[test]
fn simple_local_i128_ordered_compares_emit_signed_and_unsigned_limb_checks_at_o0() {
    let asm = capture_text(
        CaptureRequest {
            input: fixture("integer16_ordered_branch.f90"),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O0,
        },
        Stage::Asm,
    );

    assert!(
        asm.contains("cset w10, lt"),
        "backend should use signed high-limb compare for i128 lt/le:\n{}",
        asm
    );
    assert!(
        asm.contains("cset w8, lo"),
        "backend should use unsigned low-limb compare for strict ordered i128 compares:\n{}",
        asm
    );
    assert!(
        asm.contains("cset w8, ls"),
        "backend should use unsigned low-limb <= compare for i128 le:\n{}",
        asm
    );
    assert!(
        asm.contains("cset w10, gt"),
        "backend should use signed high-limb compare for i128 gt/ge:\n{}",
        asm
    );
    assert!(
        asm.contains("cset w8, hi"),
        "backend should use unsigned low-limb > compare for i128 gt:\n{}",
        asm
    );
    assert!(
        asm.contains("cset w8, hs"),
        "backend should use unsigned low-limb >= compare for i128 ge:\n{}",
        asm
    );
}

#[test]
fn simple_local_i128_ordered_branch_runs_at_o0() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_memory_backend test=simple_local_i128_ordered_branch_runs_at_o0 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let result = capture_from_path(&CaptureRequest {
        input: fixture("integer16_ordered_branch.f90"),
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O0,
    })
    .expect("ordered i128 branch program should run");

    let run = result
        .get(Stage::Run)
        .and_then(CapturedStage::as_run)
        .expect("missing run capture");

    assert_eq!(
        run.exit_code, 0,
        "expected successful ordered branch run:\n{:#?}",
        run
    );
    assert!(
        run.stdout.contains('4'),
        "ordered i128 branch program should print score 4:\n{}",
        run.stdout
    );
}

#[test]
fn pointer_to_i128_storage_round_trips_through_c_ptr_at_o0() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_memory_backend test=pointer_to_i128_storage_round_trips_through_c_ptr_at_o0 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let result = capture_from_path(&CaptureRequest {
        input: fixture("integer16_pointer_roundtrip.f90"),
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O0,
    })
    .expect("i128 pointer roundtrip program should run");

    let run = result
        .get(Stage::Run)
        .and_then(CapturedStage::as_run)
        .expect("missing run capture");

    assert_eq!(
        run.exit_code, 0,
        "expected successful i128 pointer roundtrip:\n{:#?}",
        run
    );
    assert!(
        run.stdout.contains("ok"),
        "i128 pointer roundtrip should print ok:\n{}",
        run.stdout
    );
}

#[test]
fn simple_local_i128_select_lowers_to_ir_select_and_pair_csel_at_o0() {
    let ir = capture_text(
        CaptureRequest {
            input: fixture("integer16_select.f90"),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );
    assert!(
        ir.contains("select"),
        "simple integer(16) diamond should lower to IR select:\n{}",
        ir
    );

    let asm = capture_text(
        CaptureRequest {
            input: fixture("integer16_select.f90"),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O0,
        },
        Stage::Asm,
    );
    assert_eq!(
        asm.matches("csel x").count(),
        2,
        "wide integer(16) select should lower to one CSEL per limb:\n{}",
        asm
    );
}

#[test]
fn simple_local_i128_select_runs_at_o0() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_memory_backend test=simple_local_i128_select_runs_at_o0 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let result = capture_from_path(&CaptureRequest {
        input: fixture("integer16_select.f90"),
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O0,
    })
    .expect("integer(16) select program should run");

    let run = result
        .get(Stage::Run)
        .and_then(CapturedStage::as_run)
        .expect("missing run capture");

    assert_eq!(
        run.exit_code, 0,
        "expected successful integer(16) select run:\n{:#?}",
        run
    );
    assert!(
        run.stdout.contains('1'),
        "integer(16) select program should print score 1:\n{}",
        run.stdout
    );
}

#[test]
fn simple_local_i128_select_object_snapshot_is_deterministic_at_o0() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_memory_backend test=simple_local_i128_select_object_snapshot_is_deterministic_at_o0 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("integer16_select.f90");
    let first = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O0,
        },
        Stage::Obj,
    );
    let second = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O0,
        },
        Stage::Obj,
    );

    assert_eq!(
        first, second,
        "integer(16) select object snapshots should be deterministic at O0"
    );
}

#[test]
fn simple_internal_i128_call_uses_pair_arg_and_return_regs_at_o0() {
    let asm = capture_text(
        CaptureRequest {
            input: fixture("integer16_internal_call.f90"),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O0,
        },
        Stage::Asm,
    );

    assert!(
        asm.contains("bl _afs_internal_"),
        "internal integer(16) call should branch to the internalized contained helper:\n{}",
        asm
    );
    assert!(
        asm.matches("stp x0, x1").count() >= 2,
        "internal integer(16) ABI should spill pair-register args/results with STP x0, x1:\n{}",
        asm
    );
    assert!(
        asm.matches("ldp x0, x1").count() >= 2,
        "internal integer(16) ABI should load pair-register args/results with LDP x0, x1:\n{}",
        asm
    );
}

#[test]
fn simple_internal_i128_call_runs_at_o0() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_memory_backend test=simple_internal_i128_call_runs_at_o0 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let result = capture_from_path(&CaptureRequest {
        input: fixture("integer16_internal_call.f90"),
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O0,
    })
    .expect("internal integer(16) call program should run");

    let run = result
        .get(Stage::Run)
        .and_then(CapturedStage::as_run)
        .expect("missing run capture");

    assert_eq!(
        run.exit_code, 0,
        "expected successful internal integer(16) call run:\n{:#?}",
        run
    );
    assert!(
        run.stdout.contains('1'),
        "internal integer(16) call program should print score 1:\n{}",
        run.stdout
    );
}

#[test]
fn simple_internal_i128_call_object_snapshot_is_deterministic_at_o0() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_memory_backend test=simple_internal_i128_call_object_snapshot_is_deterministic_at_o0 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("integer16_internal_call.f90");
    let first = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O0,
        },
        Stage::Obj,
    );
    let second = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O0,
        },
        Stage::Obj,
    );

    assert_eq!(
        first, second,
        "internal integer(16) call object snapshots should be deterministic at O0"
    );
}

#[test]
fn simple_local_i128_object_snapshot_is_deterministic_at_o0() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_memory_backend test=simple_local_i128_object_snapshot_is_deterministic_at_o0 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("integer16_local_roundtrip.f90");
    let first = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O0,
        },
        Stage::Obj,
    );
    let second = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O0,
        },
        Stage::Obj,
    );

    assert_eq!(
        first, second,
        "simple local i128 object snapshots should be deterministic at O0"
    );
}
