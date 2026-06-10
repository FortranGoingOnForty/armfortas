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
fn external_i128_call_uses_pair_arg_and_return_regs_at_o0() {
    let asm = capture_text(
        CaptureRequest {
            input: fixture("integer16_external_call.f90"),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O0,
        },
        Stage::Asm,
    );

    assert!(
        asm.contains("bl _add_ext"),
        "external integer(16) call should branch to the declared symbol:\n{}",
        asm
    );
    assert!(
        asm.matches("ldp x0, x1").count() >= 1,
        "external integer(16) ABI should load the outgoing pair-register arg with LDP x0, x1:\n{}",
        asm
    );
    assert!(
        asm.matches("stp x0, x1").count() >= 1,
        "external integer(16) ABI should spill the returned pair-register value with STP x0, x1:\n{}",
        asm
    );
}

#[test]
fn external_i128_call_object_snapshot_tracks_external_symbol_at_o0() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_external_backend test=external_i128_call_object_snapshot_tracks_external_symbol_at_o0 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let obj = capture_text(
        CaptureRequest {
            input: fixture("integer16_external_call.f90"),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O0,
        },
        Stage::Obj,
    );

    assert!(
        obj.contains("external _add_ext"),
        "object snapshot should preserve the unresolved external integer(16) symbol:\n{}",
        obj
    );
}

#[test]
fn external_i128_call_object_snapshot_is_deterministic_at_o0() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_external_backend test=external_i128_call_object_snapshot_is_deterministic_at_o0 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("integer16_external_call.f90");
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
        "external integer(16) call object snapshots should be deterministic at O0"
    );
}
