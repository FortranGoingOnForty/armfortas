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

fn capture_run_stdout(request: CaptureRequest) -> String {
    let result = capture_from_path(&request).expect("capture should succeed");
    match result.get(Stage::Run) {
        Some(CapturedStage::Run(run)) => run.stdout.clone(),
        _ => panic!("missing run stage"),
    }
}

fn line_fields(line: &str) -> Vec<&str> {
    line.split_whitespace().collect()
}

#[test]
fn o0_scalar_minmax_binary_fixture_runs_correctly() {
    if let Err(reason) = armfortas::testing::native_vectorizer_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_minmax_binary test=o0_scalar_minmax_binary_fixture_runs_correctly count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("do_loop_vectorize_minmax_binary.f90");

    let stdout = capture_run_stdout(CaptureRequest {
        input: source,
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O0,
    });
    let trimmed: Vec<&str> = stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(trimmed.len(), 6, "expected six output lines:\n{}", stdout);
    assert_eq!(
        line_fields(trimmed[0]),
        ["32", "17", "17", "32"],
        "i32 max wrong: {:?}",
        trimmed[0]
    );
    assert_eq!(
        line_fields(trimmed[1]),
        ["1", "16", "16", "1"],
        "i32 min wrong: {:?}",
        trimmed[1]
    );
    assert!(
        trimmed[2].contains("1.7000000E1"),
        "f32 max center lane wrong: {:?}",
        trimmed[2]
    );
    assert!(
        trimmed[3].contains("1.6000000E1"),
        "f32 min center lane wrong: {:?}",
        trimmed[3]
    );
    assert!(
        trimmed[4].contains("1.700000000000000E1"),
        "f64 max center lane wrong: {:?}",
        trimmed[4]
    );
    assert!(
        trimmed[5].contains("1.600000000000000E1"),
        "f64 min center lane wrong: {:?}",
        trimmed[5]
    );
}

#[test]
fn o3_vectorizes_elementwise_minmax_binary() {
    if let Err(reason) = armfortas::testing::native_vectorizer_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_minmax_binary test=o3_vectorizes_elementwise_minmax_binary count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("do_loop_vectorize_minmax_binary.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    // Three vmax (i32, f32, f64) and three vmin (i32, f32, f64).
    // x86 lowers i32 min/max through the SSE2 pcmpgtd blend synthesis.
    assert_eq!(
        o3_ir.matches("vmax").count(),
        3,
        "expected three vmax:\n{}",
        o3_ir
    );
    assert_eq!(
        o3_ir.matches("vmin").count(),
        3,
        "expected three vmin:\n{}",
        o3_ir
    );

    let stdout = capture_run_stdout(CaptureRequest {
        input: source,
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O3,
    });
    let trimmed: Vec<&str> = stdout
        .lines()
        .map(|l| l.trim())
        .filter(|l| !l.is_empty())
        .collect();
    assert_eq!(trimmed.len(), 6, "expected six output lines:\n{}", stdout);
    // i32 max @ {1,16,17,32} => {32,17,17,32}; i32 min => {1,16,16,1}.
    assert_eq!(
        line_fields(trimmed[0]),
        ["32", "17", "17", "32"],
        "i32 max wrong: {:?}",
        trimmed[0]
    );
    assert_eq!(
        line_fields(trimmed[1]),
        ["1", "16", "16", "1"],
        "i32 min wrong: {:?}",
        trimmed[1]
    );
    // f32 / f64 max start "3.2"; min start "1.".
    for max_line in [trimmed[2], trimmed[4]] {
        assert!(max_line.starts_with("3.2"), "fp max wrong: {:?}", max_line);
    }
    for min_line in [trimmed[3], trimmed[5]] {
        assert!(min_line.starts_with("1."), "fp min wrong: {:?}", min_line);
    }
}
