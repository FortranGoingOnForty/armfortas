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
fn o3_vectorizes_array_plus_invariant_scalar_loop() {
    if let Err(reason) = armfortas::testing::native_vectorizer_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_invariant_scalar test=o3_vectorizes_array_plus_invariant_scalar_loop count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("do_loop_vectorize_scalar.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );

    // Either form is acceptable as long as the scalar+invariant
    // pattern was vectorized:
    //   * NeonVectorize: vbroadcast in preheader + vload/vadd/vstore in body.
    //   * Vectorize fallback: afs_array_add_scalar_i32 kernel call.
    let neon = o3_ir.contains("vbroadcast") && o3_ir.contains("vadd") && o3_ir.contains("vstore");
    let kernel = o3_ir.contains("call @afs_array_add_scalar_i32(");
    assert!(
        neon || kernel,
        "O3 should vectorize a(i) = b(i) + scale (vbroadcast/vadd/vstore or afs_array_add_scalar_i32):\n{}",
        o3_ir
    );
    // Prefer the real NEON path: it avoids the runtime call entirely.
    assert!(
        neon,
        "O3 should pick the NeonVectorize broadcast path over the runtime kernel:\n{}",
        o3_ir
    );

    // Also verify runtime correctness: a(1) = 1+7 = 8, a(32) = 32+7 = 39.
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
        vec!["8", "39"],
        "vectorized a(i) = b(i) + scale should produce 8 then 39:\n{}",
        stdout
    );
}
