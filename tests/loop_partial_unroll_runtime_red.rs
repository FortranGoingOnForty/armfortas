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
fn o2_partial_unrolls_runtime_trip_reduction_with_remainder() {
    let source = fixture("loop_partial_unroll_runtime_red.f90");
    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    // The reduction loop's remainder header takes 2 params (iv +
    // acc); the iv-only store loop's remainder header takes only 1.
    // Both should appear in the IR.
    let two_param_remain = opt_ir
        .lines()
        .filter(|l| l.contains("partial_remain_header"))
        .filter(|l| l.matches(": i32").count() == 2)
        .count();
    assert!(
        two_param_remain >= 1,
        "expected a 2-param partial_remain_header (iv + acc):\n{}",
        opt_ir
    );

    let stdout = capture_run_stdout(CaptureRequest {
        input: source,
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O2,
    });
    let trimmed: Vec<&str> = stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(trimmed.len(), 1, "expected one print line:\n{}", stdout);
    assert_eq!(
        trimmed[0], "35 14910 1225",
        "runtime reduction partial unroll wrong: {:?}",
        trimmed[0]
    );
}
