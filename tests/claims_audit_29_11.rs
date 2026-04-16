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

fn function_section<'a>(ir: &'a str, name: &str) -> &'a str {
    let header = format!("  func @{}", name);
    let start = ir
        .find(&header)
        .unwrap_or_else(|| panic!("missing function section for {}", name));
    let rest = &ir[start..];
    let end = rest
        .find("\n  }\n")
        .unwrap_or_else(|| panic!("unterminated function section for {}", name));
    &rest[..end + "\n  }".len()]
}

fn param_count(func_section: &str) -> usize {
    let header = func_section.lines().next().expect("function header");
    let inside = header
        .split_once('(')
        .and_then(|(_, tail)| tail.split_once(") ->"))
        .map(|(params, _)| params.trim())
        .expect("function header params");
    if inside.is_empty() {
        0
    } else {
        inside.split(", ").count()
    }
}

#[test]
fn o0_realworld_elemental_stage_proves_elemental_and_concurrent_lowering() {
    let source = fixture("realworld_elemental_stage.f90");

    let raw_ir = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );

    assert!(
        raw_ir.contains("doconc_check_"),
        "whole-array ELEMENTAL lowering should still synthesize a DO CONCURRENT loop:\n{}",
        raw_ir
    );
    assert!(
        raw_ir.contains("call @mix(") || raw_ir.contains("call @func_"),
        "raw IR should still call the scalar ELEMENTAL body per element:\n{}",
        raw_ir
    );
    assert!(
        raw_ir.contains("call @afs_array_add_i32("),
        "the clean DO CONCURRENT combine should redirect through the bulk runtime kernel:\n{}",
        raw_ir
    );
}

#[test]
fn o2_realworld_ipo_chain_trims_dead_arg_and_removes_trivial_wrapper() {
    let source = fixture("realworld_ipo_chain.f90");

    let raw_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );
    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr, Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    let obj_a = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::Obj,
    );
    let obj_b = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::Obj,
    );

    let raw_mix = function_section(&raw_ir, "mix_step");
    assert_eq!(
        param_count(raw_mix),
        3,
        "raw helper should keep the live arg, constant arg, and dead arg before IPO:\n{}",
        raw_mix
    );
    assert!(
        raw_ir.contains("func @passthrough"),
        "raw IR should still materialize the trivial wrapper helper:\n{}",
        raw_ir
    );

    if opt_ir.contains("func @mix_step") {
        let opt_mix = function_section(&opt_ir, "mix_step");
        assert_eq!(
            param_count(opt_mix),
            2,
            "optimized helper should at least trim the dead dummy from the real-world helper chain:\n{}",
            opt_mix
        );
    }
    assert!(
        !opt_ir.contains("func @passthrough"),
        "optimized IR should remove the trivial wrapper helper:\n{}",
        opt_ir
    );
    assert_eq!(
        obj_a, obj_b,
        "IPO-audited O2 object snapshot should stay deterministic"
    );
}

#[test]
fn o2_unrolls_realworld_small_do_concurrent_kernel() {
    let source = fixture("realworld_doconc_square.f90");

    let raw_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );
    let opt_ir = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );

    assert!(
        raw_ir.contains("doconc_check_")
            && raw_ir.contains("doconc_body_")
            && raw_ir.contains("doconc_incr_"),
        "raw IR should preserve the real-world DO CONCURRENT loop identity:\n{}",
        raw_ir
    );
    assert!(
        !opt_ir.contains("doconc_check_") && !opt_ir.contains("doconc_body_"),
        "O2 should exploit the small real-world DO CONCURRENT loop enough to erase the loop shape:\n{}",
        opt_ir
    );
}

#[test]
fn o3_vectorizes_realworld_explicit_do_stage() {
    let source = fixture("realworld_vector_stage.f90");

    let o2_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr, Stage::Asm, Stage::Obj]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    let o3_asm = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O3,
        },
        Stage::Asm,
    );
    let o3_obj_a = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O3,
        },
        Stage::Obj,
    );
    let o3_obj_b = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O3,
        },
        Stage::Obj,
    );

    assert!(
        o2_ir.matches("do_check_").count() >= 2 && !o2_ir.contains("call @afs_array_add_i32("),
        "O2 should still keep the explicit scalar loop for this real-world stage:\n{}",
        o2_ir
    );
    assert!(
        o3_ir.contains("call @afs_array_add_i32(")
            && o3_ir.matches("do_check_").count() < o2_ir.matches("do_check_").count(),
        "O3 should redirect the real-world explicit DO loop to the bulk add kernel:\n{}",
        o3_ir
    );
    assert!(
        o3_asm.contains("_afs_array_add_i32"),
        "vectorized O3 assembly should reference the bulk add kernel:\n{}",
        o3_asm
    );
    assert_eq!(
        o3_obj_a, o3_obj_b,
        "vectorized O3 object snapshot should stay deterministic"
    );
}
