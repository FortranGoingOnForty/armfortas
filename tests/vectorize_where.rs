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

#[test]
fn o3_vectorizes_where_block_masked_assign() {
    let source = fixture("do_loop_vectorize_where.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );
    // The diamond should be rewritten into a single body with vload a,
    // vload b_old, vfcmp, vselect, vstore.
    assert!(o3_ir.contains("vselect"), "expected vselect:\n{}", o3_ir);
    assert!(o3_ir.contains("vfcmp"), "expected vfcmp:\n{}", o3_ir);
    // Two vloads in the WHERE body: source `a` and old `b`.
    assert!(
        o3_ir.matches("vload").count() >= 2,
        "expected >= 2 vload:\n{}",
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
    assert_eq!(trimmed.len(), 1, "expected one output line:\n{}", stdout);
    // b(1)=1 (mask=false), b(16)=16 (mask=false, a(16)=0), b(17)=1
    // (mask=true, b ← a = 17-16 = 1), b(32)=16 (mask=true, b ← a = 16).
    assert!(
        trimmed[0].contains("1.0000000E0"),
        "missing b(1)=1: {:?}",
        trimmed[0]
    );
    assert!(
        trimmed[0].contains("1.6000000E1"),
        "missing b(16)=16 / b(32)=16: {:?}",
        trimmed[0]
    );
    // Order: b(1)=1, b(16)=16, b(17)=1, b(32)=16.
    let expected = "1.0000000E0     1.6000000E1     1.0000000E0     1.6000000E1";
    assert_eq!(trimmed[0], expected, "WHERE result wrong: {:?}", trimmed[0]);
}
