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
fn o3_vectorizes_where_with_binop_invariant_scalar_body() {
    if let Err(reason) = armfortas::testing::native_vectorizer_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_where_binop test=o3_vectorizes_where_with_binop_invariant_scalar_body count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("do_loop_vectorize_where_binop.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    // Two WHERE blocks → two vselects + two vbroadcasts (one for the
    // scalar in each binop) plus the threshold broadcast(s).
    assert_eq!(
        o3_ir.matches("vselect").count(),
        2,
        "expected 2 vselect:\n{}",
        o3_ir
    );
    assert!(
        o3_ir.contains("vadd"),
        "expected vadd lifted from binop:\n{}",
        o3_ir
    );
    assert!(
        o3_ir.contains("vmul"),
        "expected vmul lifted from binop:\n{}",
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
    assert_eq!(trimmed.len(), 2, "expected two output lines:\n{}", stdout);
    let add_line = "1.0000000E0     1.6000000E1     1.0100000E2     1.1600000E2";
    let mul_line = "1.0000000E0     1.6000000E1     1.0000000E1     1.6000000E2";
    assert_eq!(trimmed[0], add_line, "WHERE add wrong: {:?}", trimmed[0]);
    assert_eq!(trimmed[1], mul_line, "WHERE mul wrong: {:?}", trimmed[1]);
}
