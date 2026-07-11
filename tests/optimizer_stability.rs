use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

fn unique_source_path(stem: &str) -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "armfortas_optimizer_stability_{}_{}_{}.f90",
        stem,
        std::process::id(),
        id
    ))
}

fn repeated_inline_source(call_count: usize) -> String {
    let mut source = String::from("program p\n  implicit none\n  integer :: x\n  x = 0\n");
    for _ in 0..call_count {
        source.push_str("  x = inc(x)\n");
    }
    source.push_str(
        "  print *, x\ncontains\n  integer function inc(y)\n    integer :: y\n    inc = y + 1\n  end function inc\nend program p\n",
    );
    source
}

#[test]
fn o1_inlines_more_than_thirty_two_sites_without_truncation() {
    let source_path = unique_source_path("inline_40");
    fs::write(&source_path, repeated_inline_source(40)).expect("write inline source");

    let request = CaptureRequest {
        input: source_path.clone(),
        requested: BTreeSet::from([Stage::OptIr]),
        opt_level: OptLevel::O1,
    };
    let result = capture_from_path(&request).expect("O1 capture should converge");
    let _ = fs::remove_file(&source_path);
    let ir = match result.get(Stage::OptIr) {
        Some(CapturedStage::Text(ir)) => ir,
        other => panic!("expected optimized IR, got {other:?}"),
    };

    let residual = ir
        .lines()
        .filter(|line| line.contains("call @afs_internal_"))
        .count();
    assert_eq!(
        residual, 0,
        "all 40 eligible contained calls should inline in one bounded batch:\n{ir}"
    );
}
