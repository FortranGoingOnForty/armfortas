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

#[test]
fn o2_binomial_blend_scalarizes_taps_and_removes_safe_stencil_checks() {
    let source = fixture("realworld_binomial_blend.f90");

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
    let raw_sections = function_sections(&raw_ir);
    assert_eq!(
        raw_sections.len(),
        2,
        "raw IR should include the program body plus one contained blend helper:\n{}",
        raw_ir
    );
    let helper_name = function_name(raw_sections[1]);

    let raw_blend = function_section(&raw_ir, helper_name);
    let opt_blend = function_section(&opt_ir, helper_name);

    assert!(
        raw_blend.contains("alloca [i32 x 4]"),
        "raw IR should still materialize taps(4) as an aggregate before SROA:\n{}",
        raw_blend
    );
    assert!(
        raw_blend.contains("rt_call @__afs_check_bounds"),
        "raw IR should still contain stencil bounds checks before BCE:\n{}",
        raw_blend
    );
    assert!(
        !opt_blend.contains("alloca [i32 x 4]"),
        "O2 optimized IR should scalarize/remove taps(4):\n{}",
        opt_blend
    );
    assert!(
        !opt_blend.contains("rt_call @__afs_check_bounds"),
        "O2 optimized IR should eliminate safe stencil bounds checks:\n{}",
        opt_blend
    );
}

#[test]
fn realworld_shape_guard_uses_runtime_shape_queries_and_stays_deterministic() {
    let source = fixture("realworld_shape_guard.f90");

    let raw_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );

    assert!(
        raw_ir.contains("call @afs_array_size("),
        "raw IR should route SIZE(work) through the runtime shape query:\n{}",
        raw_ir
    );
    assert!(
        raw_ir.contains("call @afs_array_lbound("),
        "raw IR should route LBOUND(work, 1) through the runtime shape query:\n{}",
        raw_ir
    );
    assert!(
        raw_ir.contains("call @afs_array_ubound("),
        "raw IR should route UBOUND(work, 1) through the runtime shape query:\n{}",
        raw_ir
    );

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
            "real-world runtime-shape guard should run successfully at {:?}:\n{:#?}",
            level, run
        );
        assert!(
            run.stdout.contains("6 0 5 12 36"),
            "runtime-shape guard should preserve the descriptor-backed query results at {:?}:\n{:#?}",
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
        "descriptor-backed runtime-shape guard should have a deterministic O2 object snapshot"
    );
}
