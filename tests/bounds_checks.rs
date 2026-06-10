use std::collections::BTreeSet;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, RunCapture, Stage};

fn fixture(path: &str) -> PathBuf {
    let path = PathBuf::from(path);
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

fn capture_run(request: CaptureRequest) -> RunCapture {
    let result = capture_from_path(&request).expect("capture should succeed");
    match result.get(Stage::Run) {
        Some(CapturedStage::Run(run)) => run.clone(),
        Some(CapturedStage::Text(_)) => panic!("expected run stage"),
        None => panic!("missing requested stage {}", Stage::Run.as_str()),
    }
}

#[test]
fn lowering_inserts_bounds_checks_at_o0() {
    let ir = capture_text(
        CaptureRequest {
            input: fixture("test_programs/bounds_check_loop.f90"),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );

    assert!(
        ir.contains("rt_call @__afs_check_bounds"),
        "lowered IR should contain runtime bounds checks before optimization"
    );
}

#[test]
fn bce_removes_canonical_loop_bounds_checks_at_o2() {
    let opt_ir = capture_text(
        CaptureRequest {
            input: fixture("test_programs/bounds_check_loop.f90"),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );

    assert!(
        !opt_ir.contains("rt_call @__afs_check_bounds"),
        "O2 optimized IR should eliminate provably-safe loop bounds checks"
    );
}

#[test]
fn runtime_bounds_checks_trap_out_of_range_accesses() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=bounds_checks test=runtime_bounds_checks_trap_out_of_range_accesses count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let run = capture_run(CaptureRequest {
        input: fixture("tests/fixtures/bounds_check_oob.f90"),
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O0,
    });

    assert_ne!(
        run.exit_code, 0,
        "out-of-range access should fail at runtime"
    );
    assert!(
        run.stderr.contains("Bounds check failed"),
        "runtime trap should explain the out-of-range access, stderr was:\n{}",
        run.stderr
    );
}
