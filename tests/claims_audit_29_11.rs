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

fn function_sections(ir: &str) -> Vec<&str> {
    ir.match_indices("  func @")
        .map(|(idx, _)| {
            let rest = &ir[idx..];
            let end = rest
                .find("\n  }\n")
                .unwrap_or_else(|| panic!("unterminated function section in:\n{}", rest));
            &rest[..end + "\n  }".len()]
        })
        .collect()
}

fn function_name<'a>(func_section: &'a str) -> &'a str {
    let header = func_section.lines().next().expect("function header").trim();
    let rest = header
        .strip_prefix("func @")
        .expect("function header prefix");
    let end = rest
        .find(|ch: char| ch == ' ' || ch == '(')
        .unwrap_or(rest.len());
    &rest[..end]
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
    let raw_sections = function_sections(&raw_ir);
    assert_eq!(
        raw_sections.len(),
        2,
        "raw IR should include the program body plus one scalar ELEMENTAL helper:\n{}",
        raw_ir
    );
    let scalar_body_name = function_name(raw_sections[1]);

    assert!(
        raw_ir.contains("doconc_check_"),
        "whole-array ELEMENTAL lowering should still synthesize a DO CONCURRENT loop:\n{}",
        raw_ir
    );
    assert!(
        raw_ir.contains(&format!("call @{}(", scalar_body_name)),
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
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=claims_audit_29_11 test=o2_realworld_ipo_chain_trims_dead_arg_and_removes_trivial_wrapper count=1 reason=\"{}\"",
            reason
        );
        return;
    }
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

    let raw_sections = function_sections(&raw_ir);
    assert_eq!(
        raw_sections.len(),
        5,
        "raw IR should still include accumulate, emit_value, passthrough, and mix_step helpers:\n{}",
        raw_ir
    );
    let raw_wrapper = raw_sections[3];
    let raw_wrapper_name = function_name(raw_wrapper);
    let raw_mix = raw_sections[4];
    let raw_mix_name = function_name(raw_mix);
    assert_eq!(
        param_count(raw_mix),
        3,
        "raw helper should keep the live arg, constant arg, and dead arg before IPO:\n{}",
        raw_mix
    );
    assert!(
        param_count(raw_wrapper) == 1,
        "raw IR should still materialize the trivial wrapper helper:\n{}",
        raw_ir
    );

    if opt_ir.contains(&format!("func @{}", raw_mix_name)) {
        let opt_mix = function_section(&opt_ir, raw_mix_name);
        assert_eq!(
            param_count(opt_mix),
            2,
            "optimized helper should at least trim the dead dummy from the real-world helper chain:\n{}",
            opt_mix
        );
    }
    assert!(
        !opt_ir.contains(&format!("func @{}", raw_wrapper_name)),
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
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=claims_audit_29_11 test=o3_vectorizes_realworld_explicit_do_stage count=1 reason=\"{}\"",
            reason
        );
        return;
    }
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
        o2_ir.matches("do_check_").count() >= 2
            && !o2_ir.contains("call @afs_array_add_i32(")
            && !o2_ir.contains("vadd"),
        "O2 should still keep the explicit scalar loop for this real-world stage:\n{}",
        o2_ir
    );
    // O3 vectorization can land in either of two forms now:
    //   * The newer NeonVectorize pass rewrites the inner body to
    //     vload/vadd/vstore on 128-bit lanes (preferred — no call
    //     overhead, fewer iterations).
    //   * The older Vectorize pass redirects the whole loop to the
    //     bulk runtime kernel `afs_array_add_i32` (fallback for
    //     shapes the NEON pass does not yet handle).
    // Either is a valid "vectorization" claim for this loop; the
    // load-bearing invariant is that the explicit do_check chain
    // shrinks and the loop body becomes vector-shaped (or a kernel
    // call) instead of scalar load/iadd/store.
    let o3_neon = o3_ir.contains("vstore") && o3_ir.contains("vadd");
    let o3_kernel = o3_ir.contains("call @afs_array_add_i32(");
    // For the kernel form the loop CFG is replaced by a single call,
    // so the do_check block count drops. For the NEON form the loop
    // CFG is preserved (vector ops live inside the body), so the
    // assertion is just that the body is vector-shaped, not that
    // the CFG shrank.
    assert!(
        o3_kernel || o3_neon,
        "O3 should vectorize the real-world explicit DO loop (vload/vadd/vstore or bulk kernel call):\n{}",
        o3_ir
    );
    if o3_kernel {
        assert!(
            o3_ir.matches("do_check_").count() < o2_ir.matches("do_check_").count(),
            "kernel-form O3 should replace the explicit DO with a single call:\n{}",
            o3_ir
        );
    }
    if o3_kernel {
        assert!(
            o3_asm.contains("afs_array_add_i32"),
            "kernel-form O3 assembly should reference the bulk add kernel:\n{}",
            o3_asm
        );
    } else {
        assert!(
            o3_asm.contains("add.4s") || o3_asm.contains("ldr q") || o3_asm.contains("str q"),
            "neon-form O3 assembly should reference 128-bit vector ops:\n{}",
            o3_asm
        );
    }
    assert_eq!(
        o3_obj_a, o3_obj_b,
        "vectorized O3 object snapshot should stay deterministic"
    );
}
