use std::collections::BTreeSet;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, RunCapture, Stage};

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

fn capture_run(request: CaptureRequest) -> RunCapture {
    let result = capture_from_path(&request).expect("capture should succeed");
    match result.get(Stage::Run) {
        Some(CapturedStage::Run(run)) => run.clone(),
        Some(CapturedStage::Text(_)) => panic!("expected run stage"),
        None => panic!("missing requested stage {}", Stage::Run.as_str()),
    }
}

fn main_section<'a>(asm: &'a str) -> &'a str {
    let start = asm
        .find("\n_main:\n")
        .unwrap_or_else(|| panic!("missing _main section:\n{}", asm));
    &asm[start..]
}

#[test]
fn capture_main_wrapper_calls_program_body_not_first_helper() {
    let asm = capture_text(
        CaptureRequest {
            input: fixture("program_entry_helper.f90"),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O0,
        },
        Stage::Asm,
    );

    let main = main_section(&asm);
    assert!(
        main.contains("bl ___prog_program_entry_helper"),
        "_main should call the lowered program body:\n{}",
        main
    );
    assert!(
        !main.contains("bl _set_value"),
        "_main should not jump directly to the first helper procedure:\n{}",
        main
    );
}

#[test]
fn capture_program_entry_fixture_runs_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=program_entry_audit test=capture_program_entry_fixture_runs_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let run = capture_run(CaptureRequest {
        input: fixture("program_entry_helper.f90"),
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O2,
    });

    assert_eq!(
        run.exit_code, 0,
        "program should run successfully:\n{run:#?}"
    );
    assert!(
        run.stdout.split_whitespace().any(|field| field == "99"),
        "program body should execute and print the helper-written value:\n{}",
        run.stdout
    );
}

#[test]
fn capture_program_entry_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=program_entry_audit test=capture_program_entry_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("program_entry_helper.f90");
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
        "object snapshot should stay deterministic for helper-before-program entry wiring"
    );
}
