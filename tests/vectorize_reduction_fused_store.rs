//! Audit C1 fusion: a loop carrying both an element-wise store and a
//! reduction (`c(i)=a(i)*b(i)` alongside `dot=dot+a(i)*b(i)`) must
//! vectorize BOTH — a VStore for the store beside the VReduceSum for the
//! reduction — not drop the store (the original miscompile) and not fall
//! back to fully scalar. The `test_programs` fixture guards correctness
//! at every opt level; this guards that the fusion actually happens.

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
fn o3_fuses_elementwise_store_with_reduction() {
    if let Err(reason) = armfortas::testing::native_vectorizer_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_reduction_fused_store test=o3_fuses_elementwise_store_with_reduction count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("vec_reduction_with_elementwise_store.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );

    // Both must be vectorized in the same loop: the store as a VStore,
    // the reduction folded to a VReduceSum after the loop.
    assert!(
        o3_ir.contains("vstore"),
        "fused elementwise store should become a VStore:\n{}",
        o3_ir
    );
    assert!(
        o3_ir.contains("vreduce_sum"),
        "reduction should still fold to a VReduceSum:\n{}",
        o3_ir
    );

    // Correctness: c(i) = i * 2i, dot = sum of a(i)*b(i) = 408.
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
    assert_eq!(
        trimmed,
        vec!["c= 2 8 18 32 50 72 98 128", "dot=408"],
        "fused loop should produce correct c and dot:\n{}",
        stdout
    );
}
