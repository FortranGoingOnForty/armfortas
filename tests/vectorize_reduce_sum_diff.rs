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
fn o3_vectorizes_sum_of_differences() {
    let source = fixture("do_loop_vectorize_reduce_sum_diff.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    // Three reductions, each with one vsub of two vloads.
    assert_eq!(
        o3_ir.matches("vsub").count(),
        3,
        "expected three vsub:\n{}",
        o3_ir
    );
    assert_eq!(
        o3_ir.matches("vreduce_sum").count(),
        3,
        "expected three vreduce_sum:\n{}",
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
    assert_eq!(trimmed.len(), 3, "expected three output lines:\n{}", stdout);
    // sum((2i)-i for i=1..32) = sum(i for i=1..32) = 528.
    assert_eq!(trimmed[0], "528", "i32 sum-of-diff wrong: {:?}", trimmed[0]);
    assert!(
        trimmed[1].starts_with("5.28"),
        "f32 sum-of-diff wrong: {:?}",
        trimmed[1]
    );
    assert!(
        trimmed[2].starts_with("5.28"),
        "f64 sum-of-diff wrong: {:?}",
        trimmed[2]
    );
}
