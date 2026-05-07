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
fn o3_vectorizes_f32_body_and_assembles_clean() {
    let source = fixture("do_loop_vectorize_f32.f90");

    // The IR should contain v-ops over <4 x f32>.
    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    assert!(
        o3_ir.contains("vbroadcast") && o3_ir.contains("<4 x f32>"),
        "expected NeonVectorize to emit f32 vbroadcast/vops:\n{}",
        o3_ir
    );

    // Assembly must NOT contain `dup.4s vN, sM` (the invalid gp-form
    // for an FP scalar source) — that's what the DupEl-vs-DupGen fix
    // prevents.
    let o3_asm = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O3,
        },
        Stage::Asm,
    );
    for line in o3_asm.lines() {
        let trimmed = line.trim();
        if trimmed.starts_with("dup.4s") || trimmed.starts_with("dup.2d") {
            assert!(
                !(trimmed.contains(", s") || trimmed.contains(", d")),
                "FP-scalar VBroadcast must use the lane-dup form, not gp-dup:\n{}",
                trimmed,
            );
        }
    }

    // Runtime: a(1) = 1.0 + 1.5 = 2.5, c(32) = 32.0 * 2.0 = 64.0.
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
        trimmed[0].starts_with("2.5"),
        "a(1) should be 2.5, got {:?}",
        trimmed[0]
    );
    assert!(
        trimmed[1].starts_with("6.4"),
        "c(32) should be 64.0, got {:?}",
        trimmed[1]
    );
}
