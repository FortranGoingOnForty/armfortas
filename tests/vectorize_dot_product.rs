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
fn o3_vectorizes_manual_dot_product_loop() {
    let source = fixture("do_loop_vectorize_dot.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    // The dot-product path produces two vloads, a vmul, a vadd into
    // the vector accumulator, and a final vreduce_sum.
    let n_vload = o3_ir.matches("vload").count();
    assert!(
        n_vload >= 2,
        "dot product needs at least 2 VLoads in the body, got {}:\n{}",
        n_vload,
        o3_ir
    );
    assert!(
        o3_ir.contains("vmul") && o3_ir.contains("vadd") && o3_ir.contains("vreduce_sum"),
        "expected dot-product shape (vmul + vadd + vreduce_sum):\n{}",
        o3_ir
    );

    // Runtime: sum(i*i for i = 1..32) = 32*33*65/6 = 11440.
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
        vec!["11440"],
        "vectorized dot product should produce 11440:\n{}",
        stdout
    );
}
