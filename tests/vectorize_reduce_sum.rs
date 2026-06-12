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

fn capture_run_stdout(request: CaptureRequest) -> String {
    let result = capture_from_path(&request).expect("capture should succeed");
    match result.get(Stage::Run) {
        Some(CapturedStage::Run(run)) => run.stdout.clone(),
        _ => panic!("missing run stage"),
    }
}

#[test]
fn o3_vectorizes_manual_sum_reduction_loop() {
    if let Err(reason) = armfortas::testing::native_vectorizer_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_reduce_sum test=o3_vectorizes_manual_sum_reduction_loop count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("do_loop_vectorize_reduce_sum.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    // The reduction path must produce a VBroadcast in the preheader,
    // VAdd of two <V x i32> vectors in the body, and a VReduceSum
    // after the loop.
    assert!(
        o3_ir.contains("vbroadcast") && o3_ir.contains("vadd") && o3_ir.contains("vreduce_sum"),
        "expected NeonVectorize reduction shape (vbroadcast + vadd + vreduce_sum):\n{}",
        o3_ir
    );

    // Assembly must use `mov.16b` for the loop-param transfer rather
    // than `fmov d` (which would clobber the upper lanes of the V128
    // accumulator and produce a wrong sum).
    let o3_asm = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O3,
        },
        Stage::Asm,
    );
    if cfg!(target_arch = "aarch64") {
        assert!(
            o3_asm.contains("mov.16b"),
            "regalloc must materialise V128 block-param transfers via `mov.16b`, not `fmov d`:\n{}",
            o3_asm
        );
        assert!(
            o3_asm.contains("addv.4s"),
            "VReduceSum should lower to `addv.4s` for an i32 accumulator:\n{}",
            o3_asm
        );
    } else {
        // x86: the SSE2 reduce tree (pshufd + step op + movd extract)
        // is an isel detail; assert the paddd step op for the i32
        // accumulator.
        assert!(
            o3_asm.contains("paddd"),
            "VReduceSum should step through paddd for an i32 accumulator:\n{}",
            o3_asm
        );
    }

    // Runtime: sum(1..32) = 32*33/2 = 528.
    let stdout = capture_run_stdout(CaptureRequest {
        input: source,
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O3,
    });
    let trimmed: Vec<&str> = stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(
        trimmed,
        vec!["528"],
        "vectorized sum reduction should produce 528:\n{}",
        stdout
    );
}
