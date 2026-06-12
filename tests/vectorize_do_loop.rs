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
fn o3_vectorizes_full_extent_do_loop_and_keeps_objects_deterministic() {
    if let Err(reason) = armfortas::testing::native_vectorizer_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_do_loop test=o3_vectorizes_full_extent_do_loop_and_keeps_objects_deterministic count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("do_loop_vectorize.f90");

    let raw_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );
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
        raw_ir.contains("do_check_") && raw_ir.contains("store %"),
        "raw IR should keep the explicit scalar loop shape:\n{}",
        raw_ir
    );
    assert!(
        o2_ir.contains("do_check_") && !o2_ir.contains("call @afs_array_add_i32("),
        "O2 should keep the scalar loop for this ordinary DO map:\n{}",
        o2_ir
    );
    // Two valid vectorization shapes at O3:
    //   * NeonVectorize rewrites the inner body to vload/vadd/vstore
    //     on 128-bit lanes (preferred — no call overhead).
    //   * The older Vectorize pass replaces the loop with a single
    //     afs_array_add_i32 kernel call (fallback).
    let o3_neon = o3_ir.contains("vstore") && o3_ir.contains("vadd");
    let o3_kernel = o3_ir.contains("call @afs_array_add_i32(");
    assert!(
        o3_neon || o3_kernel,
        "O3 should vectorize the scalar loop (vload/vadd/vstore or bulk kernel call):\n{}",
        o3_ir
    );
    if o3_kernel {
        assert!(
            !o3_ir.contains("do_check_"),
            "kernel-form O3 should remove the loop CFG entirely:\n{}",
            o3_ir
        );
        assert!(
            o3_asm.contains("_afs_array_add_i32"),
            "kernel-form O3 assembly should reference the bulk add kernel:\n{}",
            o3_asm
        );
    } else if cfg!(target_arch = "aarch64") {
        assert!(
            o3_asm.contains("ldr q") || o3_asm.contains("add.4s") || o3_asm.contains("str q"),
            "neon-form O3 assembly should reference 128-bit vector ops:\n{}",
            o3_asm
        );
    } else {
        // x86: SseVectorize (x10) lowers the i32 add over 4-lane
        // vectors as paddd with movups loads/stores.
        assert!(
            o3_asm.contains("paddd") && o3_asm.contains("movups"),
            "sse-form O3 assembly should reference packed i32 add and unaligned moves:\n{}",
            o3_asm
        );
    }
    assert_eq!(
        o3_obj_a, o3_obj_b,
        "O3 vectorized object snapshot should stay deterministic"
    );
}
