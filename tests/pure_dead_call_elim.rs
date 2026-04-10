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
    let start = ir.find(&header).unwrap_or_else(|| panic!("missing function section for {}", name));
    let rest = &ir[start..];
    let end = rest
        .find("\n  }\n")
        .unwrap_or_else(|| panic!("unterminated function section for {}", name));
    &rest[..end + "\n  }".len()]
}

#[test]
fn o1_eliminates_unused_pure_recursive_call_from_program_entry() {
    let source = fixture("pure_dead_call.f90");

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
            opt_level: OptLevel::O1,
        },
        Stage::OptIr,
    );

    let raw_main = function_section(&raw_ir, "__prog_pure_dead_call");
    let opt_main = function_section(&opt_ir, "__prog_pure_dead_call");

    assert!(
        raw_main.contains("call @heavy_fact(") || raw_main.contains("call @func_"),
        "lowered caller should still contain the unused PURE call before optimization:\n{}",
        raw_main
    );
    assert!(
        !opt_main.contains("call @heavy_fact(") && !opt_main.contains("call @func_"),
        "O1 optimized caller should delete the dead PURE call:\n{}",
        opt_main
    );
}
