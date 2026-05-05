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
fn o3_vectorizes_fp_sum_reductions() {
    let source = fixture("do_loop_vectorize_reduce_fp.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    assert!(
        o3_ir.contains("<4 x f32>") && o3_ir.contains("<2 x f64>"),
        "expected both f32 and f64 vector accumulators in IR:\n{}",
        o3_ir
    );
    assert_eq!(
        o3_ir.matches("vreduce_sum").count(),
        2,
        "expected two vreduce_sums (one per FP loop):\n{}",
        o3_ir
    );

    let o3_asm = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O3,
        },
        Stage::Asm,
    );
    // f32 reduce: faddp.4s + faddp.2s pair.
    // f64 reduce: faddp.2d (single step).
    assert!(
        o3_asm.contains("faddp.4s") && o3_asm.contains("faddp.2s"),
        "f32 reduce should use the two-step faddp pair:\n{}",
        o3_asm
    );
    assert!(
        o3_asm.contains("faddp.2d"),
        "f64 reduce should use faddp.2d:\n{}",
        o3_asm
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
    // f32: 5.28E2, f64: 5.28E2 with more precision. Both should
    // start with "5.28".
    assert!(
        trimmed[0].starts_with("5.28"),
        "s32 should be 528, got {:?}",
        trimmed[0]
    );
    assert!(
        trimmed[1].starts_with("5.28"),
        "s64 should be 528, got {:?}",
        trimmed[1]
    );
}
