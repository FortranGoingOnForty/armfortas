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
fn o3_vectorizes_where_with_array_plus_array_body() {
    if let Err(reason) = armfortas::testing::native_vectorizer_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_where_2arr test=o3_vectorizes_where_with_array_plus_array_body count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("do_loop_vectorize_where_2arr.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    // The array+array WHERE body must lower to two vloads (a and b)
    // feeding a vadd that drives vselect's true arm.
    assert_eq!(
        o3_ir.matches("vselect").count(),
        1,
        "expected exactly one vselect:\n{}",
        o3_ir
    );
    assert!(
        o3_ir.matches("vload").count() >= 3,
        "expected >=3 vload (a, dest, b):\n{}",
        o3_ir
    );
    assert!(
        o3_ir.contains("vadd"),
        "expected vadd lifted from a+b in WHERE body:\n{}",
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
    assert_eq!(trimmed.len(), 1, "expected one output line:\n{}", stdout);
    assert_eq!(
        trimmed[0], "1.0000000E0     1.6000000E1     1.0100000E2     1.1600000E2",
        "array+array WHERE wrong: {:?}",
        trimmed[0]
    );
}
