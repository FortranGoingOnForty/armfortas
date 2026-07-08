use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn temp_source(name: &str, functions: usize) -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "afs_{}_{}_{}_{}.f90",
        name,
        std::process::id(),
        id,
        functions
    ));
    fs::write(&path, inline_chain_source(functions)).expect("write generated source");
    path
}

fn inline_chain_source(functions: usize) -> String {
    assert!(functions >= 2);

    let mut src = String::new();
    src.push_str("module inline_chain_m\n  implicit none\ncontains\n");
    src.push_str("  integer function p0()\n    p0 = 0\n  end function p0\n");

    for idx in 1..functions {
        src.push_str(&format!(
            "  integer function p{}()\n    p{} = p{}() + 1\n  end function p{}\n",
            idx,
            idx,
            idx - 1,
            idx
        ));
    }

    src.push_str("end module inline_chain_m\n");
    src
}

fn compile_opt_ir(path: PathBuf) -> Duration {
    let start = Instant::now();
    let result = capture_from_path(&CaptureRequest {
        input: path.clone(),
        requested: BTreeSet::from([Stage::OptIr]),
        opt_level: OptLevel::O2,
    })
    .unwrap_or_else(|err| {
        panic!(
            "optimized IR capture failed for {}: {}",
            path.display(),
            err
        )
    });
    let elapsed = start.elapsed();

    match result.get(Stage::OptIr) {
        Some(CapturedStage::Text(ir)) if ir.contains("func @") => {}
        Some(other) => panic!(
            "unexpected optimized IR capture for {}: {:?}",
            path.display(),
            other
        ),
        None => panic!("missing optimized IR capture for {}", path.display()),
    }

    let _ = fs::remove_file(path);
    elapsed
}

#[test]
fn inline_chain_compile_time_scales_below_quadratic() {
    let warmup = temp_source("inline_warmup", 20);
    let _ = compile_opt_ir(warmup);

    let small = compile_opt_ir(temp_source("inline_small", 100));
    let large = compile_opt_ir(temp_source("inline_large", 200));
    let ceiling = small.mul_f64(4.0) + Duration::from_secs(3);

    assert!(
        large < ceiling,
        "O2 compile time for a 200-function inline chain should stay under a quadratic ceiling: \
         small={:?}, large={:?}, ceiling={:?}",
        small,
        large,
        ceiling
    );
}
