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
fn o3_vectorizes_minmax_with_unary_load() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_reduce_minmax_unary test=o3_vectorizes_minmax_with_unary_load count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("do_loop_vectorize_reduce_minmax_unary.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    // Expect vabs lifted into the body, plus one vreduce_max and
    // one vreduce_min.
    assert!(o3_ir.contains("vabs"), "expected vabs in IR:\n{}", o3_ir);
    assert_eq!(
        o3_ir.matches("vreduce_max").count(),
        1,
        "expected one vreduce_max:\n{}",
        o3_ir
    );
    assert_eq!(
        o3_ir.matches("vreduce_min").count(),
        1,
        "expected one vreduce_min:\n{}",
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
    assert_eq!(trimmed.len(), 2, "expected two output lines:\n{}", stdout);
    assert!(
        trimmed[0].starts_with("1.6"),
        "max abs wrong: got {:?}",
        trimmed[0]
    );
    assert!(
        trimmed[1].starts_with("0."),
        "min abs wrong: got {:?}",
        trimmed[1]
    );
}
