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
fn o3_vectorizes_sum_reductions_with_scalar_tail() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_reduce_sum_tail test=o3_vectorizes_sum_reductions_with_scalar_tail count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("do_loop_vectorize_reduce_sum_tail.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    // All four reductions should leave a vreduce_sum at exit
    // followed by peeled scalar iadd/fadd ops chaining from the
    // reduce result.
    assert_eq!(
        o3_ir.matches("vreduce_sum").count(),
        4,
        "expected four vreduce_sum (i32, i64, f32, f64):\n{}",
        o3_ir
    );
    assert!(
        o3_ir.contains("<4 x i32>")
            && o3_ir.contains("<2 x i64>")
            && o3_ir.contains("<4 x f32>")
            && o3_ir.contains("<2 x f64>"),
        "expected i32/i64/f32/f64 vector accumulators in IR:\n{}",
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
    // 1 + 2 + ... + 31 = 31 * 32 / 2 = 496.
    assert_eq!(trimmed[0], "496", "i32 sum wrong: got {:?}", trimmed[0]);
    assert_eq!(trimmed[1], "496", "i64 sum wrong: got {:?}", trimmed[1]);
    assert!(
        trimmed[2].starts_with("4.96"),
        "f32 sum wrong: got {:?}",
        trimmed[2]
    );
    assert!(
        trimmed[3].starts_with("4.96"),
        "f64 sum wrong: got {:?}",
        trimmed[3]
    );
}
