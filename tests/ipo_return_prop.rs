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
    let rest = header
        .strip_prefix("func @")
        .expect("function header prefix");
    let end = rest
        .find(|ch: char| ch == ' ' || ch == '(')
        .unwrap_or(rest.len());
    &rest[..end]
}

fn call_arg_counts_for(func_section: &str, callee_name: &str) -> Vec<usize> {
    func_section
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let call = line.find(&format!("call @{}", callee_name))?;
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
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=ipo_return_prop test=o2_propagates_trivial_return_and_deletes_helper count=1 reason=\"{}\"",
            reason
        );
        return;
    }
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

    let raw_sections = function_sections(&raw_ir);
    assert_eq!(
        raw_sections.len(),
        2,
        "raw IR should include the program body plus one contained helper:\n{}",
        raw_ir
    );
    let raw_main = raw_sections[0];
    let raw_helper = raw_sections[1];
    let raw_helper_name = function_name(raw_helper);

    assert_eq!(
        call_arg_counts_for(raw_main, raw_helper_name),
        vec![1, 1],
        "raw caller should still materialize helper calls:\n{}",
        raw_main
    );
    assert!(
        !opt_ir.contains(&format!("func @{}", raw_helper_name)),
        "optimized IR should remove the trivial helper entirely:\n{}",
        opt_ir
    );
    let opt_main = function_section(&opt_ir, "__prog_ipo_return_prop");
    assert!(
        !opt_main.contains("call @func_")
            && !opt_main.contains(&format!("call @{}", raw_helper_name)),
        "optimized caller should no longer call the passthrough helper:\n{}",
        opt_main
    );
    assert_eq!(
        obj_a, obj_b,
        "O2 object snapshot should stay deterministic after return propagation"
    );
}
