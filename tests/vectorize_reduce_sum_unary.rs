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
fn o3_vectorizes_sum_with_unary_load() {
    if let Err(reason) = armfortas::testing::native_vectorizer_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_reduce_sum_unary test=o3_vectorizes_sum_with_unary_load count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("do_loop_vectorize_reduce_sum_unary.f90");

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
        "expected both vneg and vabs in IR:\n{}",
        o3_ir
    );
    assert_eq!(
        o3_ir.matches("vreduce_sum").count(),
        4,
        "expected four vreduce_sum:\n{}",
        o3_ir
    );

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
    assert_eq!(trimmed.len(), 4, "expected four output lines:\n{}", stdout);
    // sum(-i for i=1..32) = -528.
    assert_eq!(trimmed[0], "-528", "neg sum wrong: got {:?}", trimmed[0]);
    // sum(|i-16| for i=1..32) = 240 + 16 = 256.
    assert!(
        trimmed[1].starts_with("2.56"),
        "f32 abs sum wrong: got {:?}",
        trimmed[1]
    );
    assert!(
        trimmed[2].starts_with("2.56"),
        "f64 abs sum wrong: got {:?}",
        trimmed[2]
    );
    // Trip = 31; head 28 + tail 3, sum |i-16| for i=1..31 = 240.
    assert!(
        trimmed[3].starts_with("2.4"),
        "f32 abs+tail wrong: got {:?}",
        trimmed[3]
    );
}
