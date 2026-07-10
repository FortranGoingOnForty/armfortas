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

fn function_name(func_section: &str) -> &str {
    let header = func_section.lines().next().expect("function header").trim();
    let rest = header
        .strip_prefix("func @")
        .expect("function header prefix");
    let end = rest.find([' ', '(']).unwrap_or(rest.len());
    &rest[..end]
}

fn param_count(func_section: &str) -> usize {
    let header = func_section.lines().next().expect("function header");
    let inside = header
        .split_once('(')
        .and_then(|(_, tail)| tail.split_once(") ->"))
        .map(|(params, _)| params.trim())
        .expect("function header params");
    if inside.is_empty() {
        0
    } else {
        inside.split(", ").count()
    }
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
fn o2_elides_dead_dummy_arg_from_recursive_internal_helper() {
    let source = fixture("ipo_dead_arg.f90");

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
        "raw IR should include the program body plus one contained helper:\n{}",
        raw_ir
    );
    let raw_main = raw_sections[0];
    let raw_helper = raw_sections[1];
    let raw_helper_name = function_name(raw_helper);
    let opt_main = function_section(&opt_ir, "__prog_ipo_dead_arg");
    let opt_helper = function_section(&opt_ir, raw_helper_name);

    assert_eq!(
        param_count(raw_helper),
        3,
        "raw helper should keep all dummy args:\n{}",
        raw_helper
    );
    assert_eq!(
        param_count(opt_helper),
        2,
        "optimized helper should drop the dead dummy arg:\n{}",
        opt_helper
    );

    let raw_call_counts = [
        call_arg_counts_for(raw_main, raw_helper_name),
        call_arg_counts_for(raw_helper, raw_helper_name),
    ]
    .concat();
    // dead-arg-elim rewrites @helper in-place (no clone, no rename)
    // so the optimized IR still calls @helper — with one fewer
    // argument per site.
    let opt_call_counts = [
        call_arg_counts_for(opt_main, raw_helper_name),
        call_arg_counts_for(opt_helper, raw_helper_name),
    ]
    .concat();

    assert_eq!(
        raw_call_counts,
        vec![3, 3],
        "raw IR should pass all three args at both call sites"
    );
    assert_eq!(
        opt_call_counts,
        vec![2, 2],
        "optimized IR should trim the dead arg at both call sites"
    );
}
