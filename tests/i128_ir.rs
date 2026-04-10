use std::collections::BTreeSet;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from("tests/fixtures").join(name);
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
fn integer16_lowers_to_i128_in_raw_ir() {
    let ir = capture_text(
        CaptureRequest {
            input: fixture("integer16_ir.f90"),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );

    assert!(
        ir.contains("i128"),
        "raw IR should expose integer(16) as i128:\n{}",
        ir
    );
    assert!(
        ir.contains("const_int 42 : i128"),
        "raw IR should materialize small kind-16 literals as i128 constants:\n{}",
        ir
    );
}
