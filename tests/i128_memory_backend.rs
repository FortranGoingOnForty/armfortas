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
fn simple_local_i128_roundtrip_emits_wide_pair_moves_at_o0() {
    let asm = capture_text(
        CaptureRequest {
            input: fixture("integer16_local_roundtrip.f90"),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O0,
        },
        Stage::Asm,
    );

    assert!(
        asm.contains("movz x16, #42"),
        "backend should materialize the low 64-bit half of the i128 constant:\n{}",
        asm
    );
    assert!(
        asm.contains("mov x17, xzr"),
        "backend should zero the high 64-bit half of the i128 constant:\n{}",
        asm
    );
    assert!(
        asm.contains("stp x16, x17"),
        "backend should store i128 values as paired 64-bit writes:\n{}",
        asm
    );
    assert!(
        asm.contains("ldp x16, x17"),
        "backend should reload i128 values as paired 64-bit reads:\n{}",
        asm
    );
}

#[test]
fn simple_local_i128_object_snapshot_is_deterministic_at_o0() {
    let source = fixture("integer16_local_roundtrip.f90");
    let first = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O0,
        },
        Stage::Obj,
    );
    let second = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O0,
        },
        Stage::Obj,
    );

    assert_eq!(
        first, second,
        "simple local i128 object snapshots should be deterministic at O0"
    );
}
