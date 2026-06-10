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
fn o3_vectorizes_head_and_peels_scalar_tail() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_scalar_tail test=o3_vectorizes_head_and_peels_scalar_tail count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("do_loop_vectorize_scalar_tail.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    // Vector head fired (vload / vstore present).
    assert!(
        o3_ir.contains("vload") && o3_ir.contains("vstore"),
        "expected vector head with vload/vstore in IR:\n{}",
        o3_ir
    );
    assert!(
        o3_ir.contains("<4 x i32>"),
        "expected i32 vector accumulator in IR:\n{}",
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
    assert_eq!(trimmed.len(), 5, "expected five output lines:\n{}", stdout);

    // total = sum(3*i for i=1..31) = 1488
    assert_eq!(trimmed[0], "1488", "wrong total, got {:?}", trimmed[0]);
    // c(1) = 1 + 2 = 3
    assert_eq!(trimmed[1], "3", "c(1) wrong, got {:?}", trimmed[1]);
    // c(28) = 28 + 56 = 84  (last lane of last vector iter)
    assert_eq!(trimmed[2], "84", "c(28) wrong, got {:?}", trimmed[2]);
    // c(29) = 29 + 58 = 87  (first scalar tail iteration)
    assert_eq!(trimmed[3], "87", "c(29) wrong, got {:?}", trimmed[3]);
    // c(31) = 31 + 62 = 93  (last scalar tail iteration)
    assert_eq!(trimmed[4], "93", "c(31) wrong, got {:?}", trimmed[4]);
}
