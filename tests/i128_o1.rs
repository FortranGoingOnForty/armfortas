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
fn o1_optir_const_folds_integer16_mul() {
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
            opt_level: OptLevel::O1,
        },
        Stage::OptIr,
    );

    assert!(
        raw_ir.contains("imul"),
        "raw integer(16) IR should still show the wide multiply before O1:\n{}",
        raw_ir
    );
    assert!(
        opt_ir.contains("const_int 42 : i128"),
        "O1 integer(16) pipeline should fold the wide multiply to a constant:\n{}",
        opt_ir
    );
    assert!(
        !opt_ir.contains("imul"),
        "O1 integer(16) pipeline should remove the wide multiply before backend:\n{}",
        opt_ir
    );
}

#[test]
fn o1_backend_runs_internal_integer16_call() {
    let result = capture_from_path(&CaptureRequest {
        input: fixture("integer16_internal_call.f90"),
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O1,
    })
    .expect("integer(16) internal call should run at O1");

    let run = result
        .get(Stage::Run)
        .and_then(CapturedStage::as_run)
        .expect("missing run capture");

    assert_eq!(run.exit_code, 0, "expected successful O1 integer(16) run:\n{:#?}", run);
    assert!(
        run.stdout.contains('1'),
        "O1 integer(16) internal call program should print score 1:\n{}",
        run.stdout
    );
}

#[test]
fn o1_integer16_object_snapshot_is_deterministic() {
    let source = fixture("integer16_mul.f90");
    let first = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O1,
        },
        Stage::Obj,
    );
    let second = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O1,
        },
        Stage::Obj,
    );

    assert_eq!(
        first, second,
        "O1 integer(16) object snapshots should stay deterministic once the wide multiply folds away"
    );
}
