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

fn contains_i128_load(ir: &str) -> bool {
    ir.lines()
        .any(|line| line.contains(" = load ") && line.trim_end().ends_with(": i128"))
}

#[test]
fn o2_optir_const_folds_integer16_mul() {
    let raw_ir = capture_text(
        CaptureRequest {
            input: fixture("integer16_mul.f90"),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );
    let opt_ir = capture_text(
        CaptureRequest {
            input: fixture("integer16_mul.f90"),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );

    assert!(
        raw_ir.contains("imul"),
        "raw integer(16) IR should still show the wide multiply before O2:\n{}",
        raw_ir
    );
    assert!(
        !opt_ir.contains("imul"),
        "O2 integer(16) pipeline should remove the wide multiply before backend:\n{}",
        opt_ir
    );
}

#[test]
fn o2_optir_promotes_branchy_integer16_local() {
    let raw_ir = capture_text(
        CaptureRequest {
            input: fixture("integer16_branchy_mem2reg.f90"),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );
    let opt_ir = capture_text(
        CaptureRequest {
            input: fixture("integer16_branchy_mem2reg.f90"),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );

    assert!(
        raw_ir.contains("alloca i128"),
        "raw integer(16) branchy IR should still materialize stack storage before O2:\n{}",
        raw_ir
    );
    assert!(
        contains_i128_load(&raw_ir),
        "raw integer(16) branchy IR should still load the local before O2 promotion:\n{}",
        raw_ir
    );
    assert!(
        !opt_ir.contains("alloca i128"),
        "O2 integer(16) pipeline should eliminate the stack slot after mem2reg promotion:\n{}",
        opt_ir
    );
    assert!(
        !contains_i128_load(&opt_ir),
        "O2 integer(16) pipeline should eliminate wide loads after promotion:\n{}",
        opt_ir
    );
}

#[test]
fn o2_backend_runs_internal_integer16_call() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_o2 test=o2_backend_runs_internal_integer16_call count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let result = capture_from_path(&CaptureRequest {
        input: fixture("integer16_internal_call.f90"),
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O2,
    })
    .expect("integer(16) internal call should run at O2");

    let run = result
        .get(Stage::Run)
        .and_then(CapturedStage::as_run)
        .expect("missing run capture");

    assert_eq!(
        run.exit_code, 0,
        "expected successful O2 integer(16) run:\n{:#?}",
        run
    );
    let stdout = run.stdout_text().expect("run stdout should be UTF-8");
    assert!(
        stdout.contains('1'),
        "O2 integer(16) internal call program should print score 1:\n{}",
        stdout
    );
}

#[test]
fn o2_integer16_object_snapshot_is_deterministic() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_o2 test=o2_integer16_object_snapshot_is_deterministic count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("integer16_mul.f90");
    let first = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::Obj,
    );
    let second = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::Obj,
    );

    assert_eq!(
        first, second,
        "O2 integer(16) object snapshots should stay deterministic"
    );
}
