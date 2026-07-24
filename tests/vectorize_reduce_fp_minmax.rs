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
        Some(CapturedStage::Run(run)) => run
            .stdout_text()
            .expect("run stdout should be valid UTF-8")
            .to_owned(),
        _ => panic!("missing run stage"),
    }
}

#[test]
fn o3_vectorizes_fp_minmax_reductions() {
    if let Err(reason) = armfortas::testing::native_vectorizer_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_reduce_fp_minmax test=o3_vectorizes_fp_minmax_reductions count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("do_loop_vectorize_reduce_fp_minmax.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    // Each min/max loop should leave behind one vmax/vmin in the
    // body and one vreduce_max/vreduce_min at exit.
    assert_eq!(
        o3_ir.matches("vreduce_max").count(),
        2,
        "expected two vreduce_max (f32 + f64):\n{}",
        o3_ir
    );
    assert_eq!(
        o3_ir.matches("vreduce_min").count(),
        2,
        "expected two vreduce_min (f32 + f64):\n{}",
        o3_ir
    );
    assert!(
        o3_ir.contains("<4 x f32>") && o3_ir.contains("<2 x f64>"),
        "expected both f32 and f64 vector accumulators in IR:\n{}",
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
    if cfg!(target_arch = "aarch64") {
        assert!(
            o3_asm.contains("fmaxv.4s"),
            "f32 max reduce should use fmaxv.4s:\n{}",
            o3_asm
        );
        assert!(
            o3_asm.contains("fminv.4s"),
            "f32 min reduce should use fminv.4s:\n{}",
            o3_asm
        );
        // f64: NEON has no fmaxv.2d, the pairwise scalar form is the
        // across-lane reduce for two f64 lanes.
        assert!(
            o3_asm.contains("fmaxp.2d"),
            "f64 max reduce should use fmaxp.2d:\n{}",
            o3_asm
        );
        assert!(
            o3_asm.contains("fminp.2d"),
            "f64 min reduce should use fminp.2d:\n{}",
            o3_asm
        );
    } else {
        // x86: SSE2 reduces through a shuffle tree; the tree shape is
        // an isel detail, so assert the step ops per element type.
        assert!(
            o3_asm.contains("maxps") && o3_asm.contains("minps"),
            "f32 min/max reduces should step through maxps/minps:\n{}",
            o3_asm
        );
        assert!(
            o3_asm.contains("maxpd") && o3_asm.contains("minpd"),
            "f64 min/max reduces should step through maxpd/minpd:\n{}",
            o3_asm
        );
    }

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
    assert!(
        trimmed[0].starts_with("3.2"),
        "f32 max should be 32.0, got {:?}",
        trimmed[0]
    );
    assert!(
        trimmed[1].starts_with("1.0"),
        "f32 min should be 1.0, got {:?}",
        trimmed[1]
    );
    assert!(
        trimmed[2].starts_with("3.2"),
        "f64 max should be 32.0, got {:?}",
        trimmed[2]
    );
    assert!(
        trimmed[3].starts_with("1.0"),
        "f64 min should be 1.0, got {:?}",
        trimmed[3]
    );
}
