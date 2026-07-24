use std::collections::BTreeSet;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

fn program(name: &str) -> PathBuf {
    let path = PathBuf::from("test_programs").join(name);
    assert!(path.exists(), "missing program fixture {}", path.display());
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
fn integer16_read_uses_wide_reader_in_ir_and_asm() {
    let source = program("integer16_read.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    assert!(
        opt_ir.contains("call @afs_read_int128("),
        "optimized IR should route integer(16) read through the wide runtime reader:\n{}",
        opt_ir
    );

    let asm = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O2,
        },
        Stage::Asm,
    );
    assert!(
        asm.contains("afs_read_int128"),
        "assembly should reference the wide runtime reader symbol:\n{}",
        asm
    );
}

#[test]
fn integer16_read_runs_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_runtime_read test=integer16_read_runs_across_all_opt_levels count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    for level in [
        OptLevel::O0,
        OptLevel::O1,
        OptLevel::O2,
        OptLevel::O3,
        OptLevel::Os,
        OptLevel::Ofast,
    ] {
        let result = capture_from_path(&CaptureRequest {
            input: program("integer16_read.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| panic!("integer(16) read should run at {:?}:\n{}", level, e));

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful integer(16) read run at {:?}:\n{:#?}",
            level, run
        );
        let stdout = run.stdout_text().expect("run stdout should be UTF-8");
        assert!(
            stdout.contains("170141183460469231731687303715884105727"),
            "wide positive integer(16) read should survive at {:?}:\n{}",
            level,
            stdout
        );
        assert!(
            stdout.contains("-170141183460469231731687303715884105727"),
            "wide negative integer(16) read should survive at {:?}:\n{}",
            level,
            stdout
        );
    }
}

#[test]
fn integer16_read_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_runtime_read test=integer16_read_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = program("integer16_read.f90");
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

    assert_eq!(
        first, second,
        "integer(16) read object snapshots should be deterministic at O2"
    );
}
