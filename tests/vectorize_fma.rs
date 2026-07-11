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
fn o0_scalar_fma_fixture_runs_correctly() {
    if let Err(reason) = armfortas::testing::native_vectorizer_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_fma test=o0_scalar_fma_fixture_runs_correctly count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("do_loop_vectorize_fma.f90");

    let stdout = capture_run_stdout(CaptureRequest {
        input: source,
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O0,
    });
    assert!(
        stdout.contains("3.3000000E1"),
        "f32 c(16) wrong at O0:\n{}",
        stdout
    );
    assert!(
        stdout.contains("3.300000000000000E1"),
        "f64 c(16) wrong at O0:\n{}",
        stdout
    );
    assert!(
        stdout.contains("2.0000000E1"),
        "broadcast FMA wrong at O0:\n{}",
        stdout
    );
}

#[test]
fn vectorized_fma_contracts_only_at_ofast() {
    if let Err(reason) = armfortas::testing::native_vectorizer_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_fma test=vectorized_fma_contracts_only_at_ofast count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("do_loop_vectorize_fma.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    assert_eq!(
        o3_ir.matches("vfma").count(),
        0,
        "O3 must keep separate vector multiply-add rounding:\n{}",
        o3_ir
    );
    assert!(
        o3_ir.matches("vmul").count() >= 3 && o3_ir.matches("vadd").count() >= 3,
        "O3 should still vectorize with separate vmul/vadd operations:\n{}",
        o3_ir
    );

    let ofast_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::Ofast,
        },
        Stage::OptIr,
    );
    let expected_fma = if cfg!(target_arch = "aarch64") { 3 } else { 0 };
    assert_eq!(
        ofast_ir.matches("vfma").count(),
        expected_fma,
        "Ofast contraction must follow native ISA capability:\n{}",
        ofast_ir
    );
    if expected_fma == 0 {
        assert!(
            ofast_ir.matches("vmul").count() >= 3 && ofast_ir.matches("vadd").count() >= 3,
            "baseline x86_64 should keep separate vector operations:\n{}",
            ofast_ir
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
    assert_eq!(trimmed.len(), 3, "expected three output lines:\n{}", stdout);
    // f32 line: c(1)=3, c(16)=33, c(32)=65.
    assert!(
        trimmed[0].starts_with("3.0000000E0"),
        "f32 c(1) wrong: {:?}",
        trimmed[0]
    );
    assert!(
        trimmed[0].contains("3.3000000E1"),
        "f32 c(16) wrong: {:?}",
        trimmed[0]
    );
    assert!(
        trimmed[0].contains("6.5000000E1"),
        "f32 c(32) wrong: {:?}",
        trimmed[0]
    );
    // f64 line.
    assert!(
        trimmed[1].starts_with("3.000000000000000E0"),
        "f64 c(1) wrong: {:?}",
        trimmed[1]
    );
    assert!(
        trimmed[1].contains("3.300000000000000E1"),
        "f64 c(16) wrong: {:?}",
        trimmed[1]
    );
    // broadcast FMA: e32(4) = 4*2.5+10 = 20.
    assert!(
        trimmed[2].starts_with("2.0000000E1"),
        "broadcast FMA wrong: {:?}",
        trimmed[2]
    );
}
