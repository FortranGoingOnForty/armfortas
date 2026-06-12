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
fn optir_respects_requested_optimization_level() {
    let source = fixture("sroa_small_array.f90");

    let o0_request = CaptureRequest {
        input: source.clone(),
        requested: BTreeSet::from([Stage::Ir, Stage::OptIr]),
        opt_level: OptLevel::O0,
    };
    let o2_request = CaptureRequest {
        input: source,
        requested: BTreeSet::from([Stage::Ir, Stage::OptIr]),
        opt_level: OptLevel::O2,
    };

    let ir_o0 = capture_text(o0_request.clone(), Stage::Ir);
    let opt_ir_o0 = capture_text(o0_request, Stage::OptIr);
    assert_eq!(
        ir_o0, opt_ir_o0,
        "O0 optimized IR capture should match the lowered IR exactly"
    );
    assert!(
        ir_o0.contains("alloca [i32 x 3]"),
        "baseline lowered IR should still show the aggregate alloca"
    );

    let ir_o2 = capture_text(o2_request.clone(), Stage::Ir);
    let opt_ir_o2 = capture_text(o2_request, Stage::OptIr);
    assert_eq!(
        ir_o2, ir_o0,
        "requested optimization level must not change the raw lowered IR capture"
    );
    assert_ne!(
        opt_ir_o2, ir_o2,
        "O2 optimized IR capture should differ from the raw lowered IR"
    );
    assert!(
        opt_ir_o2.contains("const_int 60"),
        "optimized IR should expose the folded result for the test program"
    );
}

#[test]
fn backend_capture_stages_use_optimized_ir() {
    let source = fixture("inline_pure.f90");
    // ELF symbols are bare and x86 spells the branch "call"; Mach-O
    // keeps the underscore prefix and "bl".
    let (call_mnemonic, outlined_helper) = if cfg!(target_arch = "x86_64") {
        ("call", "afs_internal___prog_inline_pure_1")
    } else {
        ("bl", "_afs_internal___prog_inline_pure_1")
    };

    let asm_o0 = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O0,
        },
        Stage::Asm,
    );
    let asm_o2 = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O2,
        },
        Stage::Asm,
    );

    assert!(
        asm_o0.contains(&format!("{} {}", call_mnemonic, outlined_helper)),
        "O0 assembly should still contain the out-of-line helper call"
    );
    assert!(
        asm_o0.contains(&format!("{}:", outlined_helper)),
        "O0 assembly should still emit the contained helper function"
    );
    assert!(
        !asm_o2.contains(&format!("{} {}", call_mnemonic, outlined_helper)),
        "O2 assembly capture should reflect inlining and remove helper calls"
    );
    assert!(
        !asm_o2.contains(&format!("{}:", outlined_helper)),
        "O2 assembly capture should not emit a now-dead outlined helper"
    );
}
