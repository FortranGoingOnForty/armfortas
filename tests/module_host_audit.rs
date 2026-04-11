use std::collections::BTreeSet;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, RunCapture, Stage};

fn fixture() -> PathBuf {
    let path = PathBuf::from("test_programs/module_global_host_assoc.f90");
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

#[test]
fn raw_ir_module_procedure_sees_shared_module_global() {
    let raw_ir = capture_text(
        CaptureRequest {
            input: fixture(),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );

    let bump = function_section(&raw_ir, "bump");

    assert!(
        bump.contains("global_addr @afs_mod_module_global_host_assoc_mod_g"),
        "module procedure should resolve g through the shared module global:\n{}",
        bump
    );
    assert!(
        bump.contains("store"),
        "module procedure should actually write the shared module global:\n{}",
        bump
    );
}

#[test]
fn module_global_host_assoc_runs_across_opt_levels_and_keeps_o2_object_deterministic() {
    let source = fixture();

    for level in [
        OptLevel::O0,
        OptLevel::O1,
        OptLevel::O2,
        OptLevel::O3,
        OptLevel::Os,
        OptLevel::Ofast,
    ] {
        let run = capture_run(CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        });
        assert_eq!(
            run.exit_code, 0,
            "module-global host association should run successfully at {:?}:\n{:#?}",
            level, run
        );
        assert!(
            run.stdout.contains("99"),
            "module-global host association should preserve the shared module write at {:?}:\n{:#?}",
            level, run
        );
    }

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

    assert_eq!(
        obj_a, obj_b,
        "module-global host association should have a deterministic O2 object snapshot"
    );
}
