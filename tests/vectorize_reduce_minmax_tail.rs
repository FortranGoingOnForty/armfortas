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
fn o3_vectorizes_minmax_reductions_with_scalar_tail() {
    let source = fixture("do_loop_vectorize_reduce_minmax_tail.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    // Two vreduce_max (i32 + f32) and two vreduce_min (i32 + f64)
    // should fire, each followed by peeled scalar select chains.
    assert_eq!(
        o3_ir.matches("vreduce_max").count(),
        2,
        "expected two vreduce_max:\n{}",
        o3_ir
    );
    assert_eq!(
        o3_ir.matches("vreduce_min").count(),
        2,
        "expected two vreduce_min:\n{}",
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
    assert_eq!(trimmed.len(), 4, "expected four output lines:\n{}", stdout);
    // a(i) = 100 - i over i=1..31 → values 99..69. Max=99, Min=69.
    assert_eq!(trimmed[0], "99", "i32 max wrong: got {:?}", trimmed[0]);
    assert_eq!(trimmed[1], "69", "i32 min wrong: got {:?}", trimmed[1]);
    assert!(
        trimmed[2].starts_with("9.9"),
        "f32 max wrong: got {:?}",
        trimmed[2]
    );
    assert!(
        trimmed[3].starts_with("-9.9"),
        "f64 min wrong: got {:?}",
        trimmed[3]
    );
}
