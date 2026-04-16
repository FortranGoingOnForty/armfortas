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
fn integer16_print_uses_wide_writer_in_ir_and_asm() {
    let source = program("integer16_print.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    assert!(
        opt_ir.contains("call @afs_write_int128("),
        "optimized IR should route integer(16) print through the wide runtime writer:\n{}",
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
        asm.contains("_afs_write_int128"),
        "assembly should reference the wide runtime writer symbol:\n{}",
        asm
    );
}

#[test]
fn integer16_print_runs_across_all_opt_levels() {
    for level in [
        OptLevel::O0,
        OptLevel::O1,
        OptLevel::O2,
        OptLevel::O3,
        OptLevel::Os,
        OptLevel::Ofast,
    ] {
        let result = capture_from_path(&CaptureRequest {
            input: program("integer16_print.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| panic!("integer(16) print should run at {:?}:\n{}", level, e));

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful integer(16) print run at {:?}:\n{:#?}",
            level, run
        );
        assert!(
            run.stdout
                .contains("170141183460469231731687303715884105727"),
            "wide positive integer(16) print should survive at {:?}:\n{}",
            level,
            run.stdout
        );
        assert!(
            run.stdout
                .contains("-170141183460469231731687303715884105727"),
            "wide negative integer(16) print should survive at {:?}:\n{}",
            level,
            run.stdout
        );
    }
}

#[test]
fn integer16_print_object_snapshot_is_deterministic_at_o2() {
    let source = program("integer16_print.f90");
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
        "integer(16) print object snapshots should be deterministic at O2"
    );
}

#[test]
fn integer16_formatted_write_uses_wide_push_in_ir_and_asm() {
    let source = program("integer16_format.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    assert!(
        opt_ir.contains("call @afs_fmt_push_int128("),
        "optimized IR should route formatted integer(16) output through the wide format push:\n{}",
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
        asm.contains("_afs_fmt_push_int128"),
        "assembly should reference the wide format push symbol:\n{}",
        asm
    );
}

#[test]
fn integer16_formatted_write_runs_across_all_opt_levels() {
    for level in [
        OptLevel::O0,
        OptLevel::O1,
        OptLevel::O2,
        OptLevel::O3,
        OptLevel::Os,
        OptLevel::Ofast,
    ] {
        let result = capture_from_path(&CaptureRequest {
            input: program("integer16_format.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| {
            panic!(
                "formatted integer(16) write should run at {:?}:\n{}",
                level, e
            )
        });

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful formatted integer(16) write run at {:?}:\n{:#?}",
            level, run
        );
        assert!(
            run.stdout
                .contains("170141183460469231731687303715884105727"),
            "wide positive formatted integer(16) output should survive at {:?}:\n{}",
            level,
            run.stdout
        );
        assert!(
            run.stdout
                .contains("-170141183460469231731687303715884105727"),
            "wide negative formatted integer(16) output should survive at {:?}:\n{}",
            level,
            run.stdout
        );
    }
}

#[test]
fn integer16_formatted_write_object_snapshot_is_deterministic_at_o2() {
    let source = program("integer16_format.f90");
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
        "formatted integer(16) write object snapshots should be deterministic at O2"
    );
}
