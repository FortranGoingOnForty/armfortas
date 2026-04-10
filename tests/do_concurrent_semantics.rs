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

#[test]
fn masked_do_concurrent_lowers_guarded_body() {
    let raw_ir = capture_text(
        CaptureRequest {
            input: fixture("do_concurrent_mask.f90"),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );

    assert!(raw_ir.contains("doconc_check_"));
    assert!(raw_ir.contains("if_then_"));
}

#[test]
fn multi_control_do_concurrent_lowers_nested_loops() {
    let raw_ir = capture_text(
        CaptureRequest {
            input: fixture("do_concurrent_multi_control.f90"),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );

    assert!(
        raw_ir.matches("doconc_check_").count() >= 2,
        "multi-control DO CONCURRENT should lower to nested concurrent loops:\n{}",
        raw_ir
    );
}
