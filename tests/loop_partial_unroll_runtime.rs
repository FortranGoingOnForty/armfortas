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
fn o2_partial_unrolls_runtime_trip_loop_with_remainder() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=loop_partial_unroll_runtime test=o2_partial_unrolls_runtime_trip_loop_with_remainder count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("loop_partial_unroll_runtime.f90");
    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    // The runtime partial-unroll path emits an `imod` in the preheader
    // for the head_bound computation, and a `partial_remain_*` block
    // for the scalar remainder loop.
    assert!(
        opt_ir.contains("imod"),
        "expected imod in preheader (head_bound computation):\n{}",
        opt_ir
    );
    assert!(
        opt_ir.contains("partial_remain_"),
        "expected partial_remain_* block (scalar remainder loop):\n{}",
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
    let fields: Vec<&str> = trimmed[0].split_whitespace().collect();
    assert_eq!(
        fields,
        vec!["33", "561", "33"],
        "runtime trip partial unroll wrong: {:?}",
        trimmed[0]
    );
}
