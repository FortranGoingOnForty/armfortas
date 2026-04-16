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
    let rest = header.strip_prefix("func @").expect("function header prefix");
    let end = rest.find(|ch: char| ch == ' ' || ch == '(').unwrap_or(rest.len());
    &rest[..end]
}

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn o2_reuses_pure_recursive_call_in_program_caller() {
    let source = fixture("pure_recursive_reuse.f90");

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
        "raw IR should include the program body plus one contained pure helper:\n{}",
        raw_ir
    );
    let helper_name = function_name(raw_sections[1]);

    let raw_main = function_section(&raw_ir, "__prog_pure_recursive_reuse");
    let opt_main = function_section(&opt_ir, "__prog_pure_recursive_reuse");

    let raw_pure_calls = count(raw_main, &format!("call @{}(", helper_name));
    let opt_pure_calls = count(opt_main, &format!("call @{}(", helper_name));

    assert_eq!(
        raw_pure_calls, 2,
        "lowered caller should materialize two recursive PURE calls before optimization:\n{}",
        raw_main
    );
    assert_eq!(
        opt_pure_calls, 1,
        "O2 optimized caller should reuse the repeated PURE call result:\n{}",
        opt_main
    );
}
