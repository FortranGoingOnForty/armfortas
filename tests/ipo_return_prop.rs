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

fn call_arg_counts_for(func_section: &str, callee_marker: &str) -> Vec<usize> {
    func_section
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let call = line.find(&format!("call {}", callee_marker))?;
            let inside = line[call..].split_once('(')?.1.split_once(')')?.0.trim();
            Some(if inside.is_empty() {
                0
            } else {
                inside.split(", ").count()
            })
        })
        .collect()
}

#[test]
fn o2_propagates_trivial_return_and_deletes_helper() {
    let source = fixture("ipo_return_prop.f90");

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
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
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

    let raw_main = function_section(&raw_ir, "__prog_ipo_return_prop");
    let _raw_helper = function_section(&raw_ir, "passthrough");

    assert_eq!(
        call_arg_counts_for(raw_main, "@passthrough"),
        vec![1, 1],
        "raw caller should still materialize helper calls:\n{}",
        raw_main
    );
    assert!(
        !opt_ir.contains("func @passthrough"),
        "optimized IR should remove the trivial helper entirely:\n{}",
        opt_ir
    );
    let opt_main = function_section(&opt_ir, "__prog_ipo_return_prop");
    assert!(
        !opt_main.contains("call @func_") && !opt_main.contains("call @passthrough"),
        "optimized caller should no longer call the passthrough helper:\n{}",
        opt_main
    );
    assert_eq!(
        obj_a, obj_b,
        "O2 object snapshot should stay deterministic after return propagation"
    );
}
