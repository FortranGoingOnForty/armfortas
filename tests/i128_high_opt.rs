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

fn high_levels() -> &'static [(OptLevel, &'static str)] {
    &[
        (OptLevel::O3, "O3"),
        (OptLevel::Os, "Os"),
        (OptLevel::Ofast, "Ofast"),
    ]
}

#[test]
fn high_opt_const_folds_integer16_mul() {
    let raw_ir = capture_text(
        CaptureRequest {
            input: fixture("integer16_mul.f90"),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );
    assert!(
        raw_ir.contains("imul"),
        "raw integer(16) IR should still show the wide multiply before high-opt folding:\n{}",
        raw_ir
    );

    for (level, label) in high_levels() {
        let opt_ir = capture_text(
            CaptureRequest {
                input: fixture("integer16_mul.f90"),
                requested: BTreeSet::from([Stage::OptIr]),
                opt_level: *level,
            },
            Stage::OptIr,
        );
        assert!(
            !opt_ir.contains("imul"),
            "{} integer(16) pipeline should remove the wide multiply before backend:\n{}",
            label,
            opt_ir
        );
    }
}

#[test]
fn high_opt_promotes_branchy_integer16_local() {
    let raw_ir = capture_text(
        CaptureRequest {
            input: fixture("integer16_branchy_mem2reg.f90"),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );
    assert!(
        raw_ir.contains("alloca"),
        "raw integer(16) branchy IR should still materialize stack storage before high-opt promotion:\n{}",
        raw_ir
    );
    assert!(
        raw_ir.contains("load"),
        "raw integer(16) branchy IR should still load the local before high-opt promotion:\n{}",
        raw_ir
    );

    for (level, label) in high_levels() {
        let opt_ir = capture_text(
            CaptureRequest {
                input: fixture("integer16_branchy_mem2reg.f90"),
                requested: BTreeSet::from([Stage::OptIr]),
                opt_level: *level,
            },
            Stage::OptIr,
        );
        assert!(
            !opt_ir.contains("alloca"),
            "{} integer(16) pipeline should eliminate the stack slot after promotion:\n{}",
            label,
            opt_ir
        );
        assert!(
            !opt_ir.contains("load"),
            "{} integer(16) pipeline should eliminate wide loads after promotion:\n{}",
            label,
            opt_ir
        );
    }
}

#[test]
fn high_opt_backend_runs_internal_integer16_call() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_high_opt test=high_opt_backend_runs_internal_integer16_call count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    for (level, label) in high_levels() {
        let result = capture_from_path(&CaptureRequest {
            input: fixture("integer16_internal_call.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: *level,
        })
        .unwrap_or_else(|e| panic!("integer(16) internal call should run at {}:\n{}", label, e));

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful {} integer(16) run:\n{:#?}",
            label, run
        );
        let stdout = run.stdout_text().expect("run stdout should be UTF-8");
        assert!(
            stdout.contains('1'),
            "{} integer(16) internal call program should print score 1:\n{}",
            label,
            stdout
        );
    }
}

#[test]
fn high_opt_integer16_object_snapshot_is_deterministic() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_high_opt test=high_opt_integer16_object_snapshot_is_deterministic count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    for (level, label) in high_levels() {
        let source = fixture("integer16_mul.f90");
        let first = capture_text(
            CaptureRequest {
                input: source.clone(),
                requested: BTreeSet::from([Stage::Obj]),
                opt_level: *level,
            },
            Stage::Obj,
        );
        let second = capture_text(
            CaptureRequest {
                input: source.clone(),
                requested: BTreeSet::from([Stage::Obj]),
                opt_level: *level,
            },
            Stage::Obj,
        );

        assert_eq!(
            first, second,
            "{} integer(16) object snapshots should stay deterministic",
            label
        );
    }
}
