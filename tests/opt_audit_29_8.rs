use std::collections::BTreeSet;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

fn fixture(path: &str) -> PathBuf {
    let path = PathBuf::from(path);
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

fn count(haystack: &str, needle: &str) -> usize {
    haystack.matches(needle).count()
}

#[test]
fn gvn_reduces_cross_block_recomputation_for_value_args() {
    let source = fixture("tests/fixtures/gvn_cross_block.f90");

    let ir_o0 = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );
    let opt_ir_o2 = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );

    assert_eq!(
        count(&ir_o0, " = iadd "),
        4,
        "lowered IR should contain four adds before optimization rewrites:\n{}",
        ir_o0
    );
    assert_eq!(
        count(&opt_ir_o2, " = iadd "),
        2,
        "O2 optimized IR should reuse the dominating a+b value across blocks:\n{}",
        opt_ir_o2
    );
    assert_eq!(
        count(&opt_ir_o2, "iadd %0, %1"),
        1,
        "optimized IR should materialize the source add only once:\n{}",
        opt_ir_o2
    );
}
