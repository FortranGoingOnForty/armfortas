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
fn globals_only_i128_backend_emits_expected_words_in_asm() {
    let asm = capture_text(
        CaptureRequest {
            input: fixture("integer16_globals_backend.f90"),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O0,
        },
        Stage::Asm,
    );

    assert!(
        asm.contains(".section __DATA,__data"),
        "asm should emit a data section for i128 globals:\n{}",
        asm
    );
    assert!(
        asm.matches(".p2align 4").count() >= 2,
        "scalar and array i128 globals should request 16-byte alignment:\n{}",
        asm
    );
    assert!(
        asm.contains(".quad 0x0000000000000000\n    .quad 0x0000000000000001"),
        "scalar i128 global should emit low/high 64-bit words:\n{}",
        asm
    );
    assert!(
        asm.contains(".quad 0x0000000000000001\n    .quad 0x0000000000000000"),
        "array i128 initializer should emit the first element's word pair:\n{}",
        asm
    );
    assert!(
        asm.contains(".quad 0x8000000000000000\n    .quad 0x0000000000000000"),
        "array i128 initializer should emit the second element's word pair:\n{}",
        asm
    );
}

#[test]
fn globals_only_i128_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_backend_data test=globals_only_i128_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("integer16_globals_backend.f90");
    let first = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::Obj,
    );
    let second = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::Obj,
    );

    assert!(
        first.contains("afs_mod_integer16_globals_backend_big_scalar"),
        "object snapshot should retain the i128 scalar global symbol:\n{}",
        first
    );
    assert!(
        first.contains("afs_mod_integer16_globals_backend_big_array"),
        "object snapshot should retain the i128 array global symbol:\n{}",
        first
    );

    assert_eq!(
        first, second,
        "globals-only i128 object snapshots should be deterministic at O2"
    );
}
