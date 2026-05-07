use std::collections::BTreeSet;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from("test_programs").join(name);
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

fn capture_run_stdout(request: CaptureRequest) -> String {
    let result = capture_from_path(&request).expect("capture should succeed");
    match result.get(Stage::Run) {
        Some(CapturedStage::Run(run)) => run.stdout.clone(),
        _ => panic!("missing run stage"),
    }
}

#[test]
fn o3_vectorizes_unary_neg_and_abs_bodies() {
    let source = fixture("do_loop_vectorize_unary.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    assert!(
        o3_ir.contains("vneg") && o3_ir.contains("vabs"),
        "expected NeonVectorize to emit both vneg and vabs:\n{}",
        o3_ir
    );

    // Runtime: see fixture comments for expected values.
    let stdout = capture_run_stdout(CaptureRequest {
        input: source,
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O3,
    });
    let trimmed: Vec<&str> = stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(trimmed.len(), 5, "expected five output lines:\n{}", stdout);
    // n(1) = -b(1) = -(1-16) = 15
    assert!(trimmed[0].starts_with("1.5"), "n(1) should be 15, got {:?}", trimmed[0]);
    // n(32) = -b(32) = -(32-16) = -16
    assert!(trimmed[1].starts_with("-1.6"), "n(32) should be -16, got {:?}", trimmed[1]);
    // a(1) = abs(-15) = 15
    assert!(trimmed[2].starts_with("1.5"), "a(1) should be 15, got {:?}", trimmed[2]);
    // a(16) = abs(0) = 0
    assert!(trimmed[3].starts_with("0.0"), "a(16) should be 0, got {:?}", trimmed[3]);
    // a(32) = abs(16) = 16
    assert!(trimmed[4].starts_with("1.6"), "a(32) should be 16, got {:?}", trimmed[4]);
}
