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
fn o0_lowers_whole_array_elemental_assignment_through_concurrent_map() {
    let source = fixture("elemental_array_map.f90");

    let raw_ir = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );

    assert!(
        raw_ir.matches("doconc_check_").count() >= 2,
        "two elemental whole-array assignments should lower through DO CONCURRENT blocks:\n{}",
        raw_ir
    );
    assert!(
        raw_ir.contains("call @shift_scale(") || raw_ir.contains("call @func_"),
        "lowered IR should still call the elemental scalar body per element:\n{}",
        raw_ir
    );
}
