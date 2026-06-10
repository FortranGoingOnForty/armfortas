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

fn function_slice<'a>(ir: &'a str, name: &str) -> &'a str {
    let marker = format!("func @{}(", name);
    let start = ir
        .find(&marker)
        .unwrap_or_else(|| panic!("missing function {} in IR:\n{}", name, ir));
    let rest = &ir[start..];
    let end = rest.find("\n  func @").unwrap_or(rest.len());
    &rest[..end]
}

fn function_sections(ir: &str) -> Vec<&str> {
    ir.match_indices("  func @")
        .map(|(idx, _)| {
            let rest = &ir[idx..];
            let end = rest.find("\n  func @").unwrap_or(rest.len());
            &rest[..end]
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
fn three_point_apply_scalarizes_coeffs_and_removes_safe_stencil_checks_at_o2() {
    let source = fixture("realworld_three_point_apply.f90");

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
        "raw IR should include the program body plus one contained apply helper:\n{}",
        raw_ir
    );
    let helper_name = function_name(raw_sections[1]);
    let raw_apply = function_slice(&raw_ir, helper_name);
    let opt_apply = function_slice(&opt_ir, helper_name);

    assert!(
        raw_apply.contains("alloca [i32 x 3]"),
        "raw IR should still materialize coeffs(3) as an aggregate before SROA:\n{}",
        raw_apply
    );
    assert!(
        raw_apply.contains("rt_call @__afs_check_bounds"),
        "raw IR should still contain stencil bounds checks before BCE:\n{}",
        raw_apply
    );
    assert!(
        !opt_apply.contains("alloca [i32 x 3]"),
        "O2 optimized IR should scalarize/remove coeffs(3):\n{}",
        opt_apply
    );
    assert!(
        !opt_apply.contains("rt_call @__afs_check_bounds"),
        "O2 optimized IR should eliminate safe stencil bounds checks:\n{}",
        opt_apply
    );
}

#[test]
fn sasum_cleanup_eliminates_chunked_loop_bounds_checks_at_o2() {
    let source = fixture("realworld_sasum_cleanup.f90");

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
        raw_ir.contains("rt_call @__afs_check_bounds"),
        "raw IR should contain chunked-loop bounds checks before BCE:\n{}",
        raw_ir
    );
    assert!(
        !opt_ir.contains("rt_call @__afs_check_bounds"),
        "O2 optimized IR should eliminate safe chunked-loop bounds checks:\n{}",
        opt_ir
    );
}

#[test]
fn realworld_29_8_kernels_have_deterministic_o2_objects() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=opt_audit_29_11 test=realworld_29_8_kernels_have_deterministic_o2_objects count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    for name in [
        "realworld_sasum_cleanup.f90",
        "realworld_three_point_apply.f90",
    ] {
        let source = fixture(name);
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
            "real-world 29.8 audit kernel should have deterministic O2 object snapshot for {}",
            name
        );
    }
}
