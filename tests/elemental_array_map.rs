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
    let rest = header.strip_prefix("func @").expect("function header prefix");
    let end = rest.find(|ch: char| ch == ' ' || ch == '(').unwrap_or(rest.len());
    &rest[..end]
}

#[test]
fn o0_lowers_whole_array_elemental_assignment_through_concurrent_map() {
    let source = fixture("elemental_array_map.f90");

    let raw_ir = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );
    let raw_sections = function_sections(&raw_ir);
    assert_eq!(
        raw_sections.len(),
        2,
        "raw IR should include the program body plus one elemental scalar helper:\n{}",
        raw_ir
    );
    let scalar_body_name = function_name(raw_sections[1]);

    assert!(
        raw_ir.matches("doconc_check_").count() >= 2,
        "two elemental whole-array assignments should lower through DO CONCURRENT blocks:\n{}",
        raw_ir
    );
    assert!(
        raw_ir.contains(&format!("call @{}(", scalar_body_name)),
        "lowered IR should still call the elemental scalar body per element:\n{}",
        raw_ir
    );
}
