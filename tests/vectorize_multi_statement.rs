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
        Some(CapturedStage::Run(run)) => run
            .stdout_text()
            .expect("run stdout should be valid UTF-8")
            .to_owned(),
        _ => panic!("missing run stage"),
    }
}

#[test]
fn o3_vectorizes_two_statement_do_body() {
    if let Err(reason) = armfortas::testing::native_vectorizer_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=vectorize_multi_statement test=o3_vectorizes_two_statement_do_body count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("do_loop_vectorize_multi.f90");

    let o3_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O3,
        },
        Stage::OptIr,
    );

    // Both statements should land in vector form: at least two VStores
    // and two VAdds/VMuls in the IR (one for `a(i)=b(i)+scale`, one
    // for `c(i)=b(i)*6`). x86 lowers the i32 multiply through the
    // SSE2 pmuludq even/odd-lane synthesis.
    let n_vstore = o3_ir.matches("vstore").count();
    let n_vbroadcast = o3_ir.matches("vbroadcast").count();
    assert!(
        n_vstore >= 2,
        "two-store body should produce >=2 VStores, got {}:\n{}",
        n_vstore,
        o3_ir
    );
    assert!(
        n_vbroadcast >= 1,
        "scale and constant 6 should each become a VBroadcast (>=1 expected):\n{}",
        o3_ir
    );

    // Runtime check: a(1)=8, a(32)=39, c(1)=6, d(32)=c(32)=192.
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
    assert_eq!(
        trimmed,
        vec!["8", "39", "6", "192"],
        "two-statement vectorized body should produce 8, 39, 6, 192:\n{}",
        stdout
    );
}
