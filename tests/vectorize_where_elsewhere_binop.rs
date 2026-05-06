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
fn o3_vectorizes_where_elsewhere_with_then_binop() {
    let source = fixture("do_loop_vectorize_where_elsewhere_binop.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    // Expect a vmul (then-arm a*scale), a vselect, and ≥3 vbroadcasts
    // (threshold + scale + elsewhere const).
    assert_eq!(
        o3_ir.matches("vselect").count(),
        1,
        "expected one vselect:\n{}",
        o3_ir
    );
    assert!(
        o3_ir.contains("vmul"),
        "expected vmul lifted from a*scale in WHERE true arm:\n{}",
        o3_ir
    );
    assert!(
        o3_ir.matches("vbroadcast").count() >= 3,
        "expected ≥3 vbroadcasts (threshold + scale + elsewhere const):\n{}",
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
        trimmed[0],
        "-1.0000000E0    -1.0000000E0     2.0000000E0     3.2000000E1",
        "WHERE/ELSEWHERE+binop wrong: {:?}",
        trimmed[0]
    );
}
