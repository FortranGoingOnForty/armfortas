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
fn optimized_i128_capture_is_available_through_ofast() {
    for level in [OptLevel::O1, OptLevel::O2, OptLevel::O3, OptLevel::Os, OptLevel::Ofast] {
        capture_from_path(&CaptureRequest {
            input: fixture("integer16_mul.f90"),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: level,
        })
        .unwrap_or_else(|e| panic!("optimized i128 capture should succeed at {:?}:\n{}", level, e));
    }
}

#[test]
fn backend_i128_capture_is_rejected_for_now() {
    let err = capture_from_path(&CaptureRequest {
        input: fixture("integer16_mul.f90"),
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
