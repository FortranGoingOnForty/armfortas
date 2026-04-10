use std::collections::BTreeSet;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, RunCapture, Stage};

fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from("tests/fixtures").join(name);
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
    let start = ir.find(&header).unwrap_or_else(|| panic!("missing function section for {}", name));
    let rest = &ir[start..];
    let end = rest
        .find("\n  }\n")
        .unwrap_or_else(|| panic!("unterminated function section for {}", name));
    &rest[..end + "\n  }".len()]
}

fn parse_last_int(stdout: &str) -> i32 {
    stdout
        .split_whitespace()
        .last()
        .unwrap_or_else(|| panic!("missing integer in stdout:\n{}", stdout))
        .parse()
        .unwrap_or_else(|_| panic!("stdout did not end in an integer:\n{}", stdout))
}

#[test]
fn ofast_reassociates_float_constant_chain_but_o3_stays_strict() {
    let source = fixture("ofast_fast_math_reassoc.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    let ofast_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::Ofast,
        },
        Stage::OptIr,
    );

    let o3_main = function_section(&o3_ir, "__prog_ofast_fast_math_reassoc");
    let ofast_main = function_section(&ofast_ir, "__prog_ofast_fast_math_reassoc");

    assert!(
        o3_main.contains("fadd") && o3_main.contains("fsub"),
        "O3 should keep the strict float chain intact:\n{}",
        o3_main
    );
    assert!(
        !ofast_main.contains("fsub"),
        "Ofast should reassociate away the subtract in the float chain:\n{}",
        ofast_main
    );

    let o3_run = capture_run(CaptureRequest {
        input: source.clone(),
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O3,
    });
    let ofast_run = capture_run(CaptureRequest {
        input: source.clone(),
        requested: BTreeSet::from([Stage::Run, Stage::Obj]),
        opt_level: OptLevel::Ofast,
    });
    let ofast_obj_a = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::Ofast,
        },
        Stage::Obj,
    );
    let ofast_obj_b = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::Ofast,
        },
        Stage::Obj,
    );

    assert_eq!(parse_last_int(&o3_run.stdout), 0, "strict O3 run should keep IEEE-style rounding loss");
    assert_eq!(parse_last_int(&ofast_run.stdout), 1, "Ofast run should expose fast-math reassociation");
    assert_eq!(ofast_obj_a, ofast_obj_b, "Ofast object snapshot should stay deterministic");
}
