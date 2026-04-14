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

fn call_arg_counts_for(func_section: &str, callee_marker: &str) -> Vec<usize> {
    func_section
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            let call = line.find(&format!("call {}", callee_marker))?;
            let inside = line[call..]
                .split_once('(')?
                .1
                .split_once(')')?
                .0
                .trim();
            Some(if inside.is_empty() { 0 } else { inside.split(", ").count() })
        })
        .collect()
}

#[test]
fn o2_specializes_constant_dummy_and_trims_internal_calls() {
    let source = fixture("ipo_const_arg.f90");

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
    let obj_o2_a = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::Obj,
    );
    let obj_o2_b = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::Obj,
    );

    let raw_main = function_section(&raw_ir, "__prog_ipo_const_arg");
    let raw_compute = function_section(&raw_ir, "compute");
    let opt_main = function_section(&opt_ir, "__prog_ipo_const_arg");
    let opt_compute = function_section(&opt_ir, "compute");

    assert_eq!(param_count(raw_compute), 2, "raw helper should keep both dummies:\n{}", raw_compute);
    assert_eq!(param_count(opt_compute), 1, "optimized helper should specialize the constant dummy:\n{}", opt_compute);
    assert!(
        opt_compute.contains("const_int 4"),
        "optimized helper should materialize the specialized constant directly:\n{}",
        opt_compute
    );
    assert!(
        !opt_compute.contains("load %1"),
        "optimized helper should stop loading the specialized dummy pointer:\n{}",
        opt_compute
    );

    assert_eq!(
        call_arg_counts_for(raw_main, "@compute"),
        vec![2, 2],
        "raw caller should pass both helper arguments at O0:\n{}",
        raw_main
    );
    // const-arg-specialize rewrites @compute in-place (no clone, no
    // rename) and dead_arg_elim drops the trimmed parameter. The
    // optimized caller therefore still calls @compute but with one
    // fewer argument per site.
    assert_eq!(
        call_arg_counts_for(opt_main, "@compute"),
        vec![1, 1],
        "optimized caller should trim the specialized constant arg:\n{}",
        opt_main
    );

    assert_eq!(obj_o2_a, obj_o2_b, "specialized O2 object snapshot should stay deterministic");
}
