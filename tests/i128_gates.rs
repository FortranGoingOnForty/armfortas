use std::collections::BTreeSet;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, FailureStage, Stage};

fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from("tests/fixtures").join(name);
    assert!(path.exists(), "missing test fixture {}", path.display());
    path
}

#[test]
fn optimized_i128_capture_is_rejected_for_now() {
    let err = capture_from_path(&CaptureRequest {
        input: fixture("integer16_add.f90"),
        requested: BTreeSet::from([Stage::OptIr]),
        opt_level: OptLevel::O2,
    })
    .expect_err("optimized i128 capture should be rejected until the opt pipeline is widened");

    assert_eq!(err.stage, FailureStage::Ir);
    assert!(
        err.detail.contains("integer(16) / i128 optimization is not yet supported"),
        "unexpected capture failure:\n{}",
        err
    );
}

#[test]
fn backend_i128_capture_is_rejected_for_now() {
    let err = capture_from_path(&CaptureRequest {
        input: fixture("integer16_add.f90"),
        requested: BTreeSet::from([Stage::Obj]),
        opt_level: OptLevel::O0,
    })
    .expect_err("backend i128 capture should be rejected until codegen lands");

    assert_eq!(err.stage, FailureStage::Ir);
    assert!(
        err.detail.contains("backend does not yet support integer(16) / i128 codegen"),
        "unexpected capture failure:\n{}",
        err
    );
}
