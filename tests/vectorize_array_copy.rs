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
fn o3_vectorizes_pure_array_copy_loop() {
    let source = fixture("do_loop_vectorize_copy.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );

    // Either NeonVectorize (vload/vstore inline, no v-binop) or the
    // older Vectorize fallback (afs_array_copy_i32) is acceptable.
    let neon = o3_ir.contains("vload")
        && o3_ir.contains("vstore")
        && !o3_ir.contains("vadd")
        && !o3_ir.contains("vsub")
        && !o3_ir.contains("vmul");
    let kernel = o3_ir.contains("call @afs_array_copy_i32(");
    assert!(
        neon || kernel,
        "O3 should vectorize a pure c(i) = b(i) copy loop:\n{}",
        o3_ir
    );
    assert!(
        neon,
        "O3 should pick the NeonVectorize copy path over the runtime kernel:\n{}",
        o3_ir
    );

    // Runtime check: c(1) = 1 and c(32) = 32 (b is filled with index).
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
        vec!["1", "32"],
        "vectorized copy should preserve element values:\n{}",
        stdout
    );
}
