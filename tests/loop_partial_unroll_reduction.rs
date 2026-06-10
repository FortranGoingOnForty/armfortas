use std::collections::BTreeSet;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from("test_programs").join(name);
    assert!(path.exists(), "missing test fixture {}", path.display());
    path
}

fn capture_run_stdout(request: CaptureRequest) -> String {
    let result = capture_from_path(&request).expect("capture should succeed");
    match result.get(Stage::Run) {
        Some(CapturedStage::Run(run)) => run.stdout.clone(),
        _ => panic!("missing run stage"),
    }
}

#[test]
fn o2_partial_unrolls_reduction_loop_correctly_threads_accumulator() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=loop_partial_unroll_reduction test=o2_partial_unrolls_reduction_loop_correctly_threads_accumulator count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("loop_partial_unroll_reduction.f90");
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
    assert_eq!(trimmed.len(), 2, "expected 2 print lines:\n{}", stdout);
    assert_eq!(trimmed[0], "136", "sum(i, 1..16) wrong: {:?}", trimmed[0]);
    assert_eq!(trimmed[1], "1496", "sum(i², 1..16) wrong: {:?}", trimmed[1]);
}
