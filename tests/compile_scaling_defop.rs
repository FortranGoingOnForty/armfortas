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
    fs::write(&path, defop_stress_source(functions)).expect("write generated source");
    path
}

fn defop_stress_source(functions: usize) -> String {
    let mut src = String::new();
    src.push_str("module defop_scan_m\n  implicit none\ncontains\n");
    for idx in 0..functions {
        src.push_str(&format!("  pure function q{idx:05}(v) result(r)\n"));
        src.push_str("    integer(8), intent(in) :: v\n    integer(8) :: r\n");
        src.push_str("    r = v\n");
        src.push_str(&format!("  end function q{idx:05}\n"));
    }
    src.push_str("end module defop_scan_m\n\n");
    src.push_str("program defop_scan\n  use defop_scan_m\n  implicit none\n  integer(8) :: acc\n");
    src.push_str("  acc = 0\n");
    for idx in 0..functions {
        src.push_str(&format!("  acc = acc + {idx}_8\n"));
    }
    src.push_str("  print '(a,i0)', 'acc=', acc\nend program defop_scan\n");
    src
}

fn compile_ir(path: PathBuf) -> Duration {
    let start = Instant::now();
    let result = capture_from_path(&CaptureRequest {
        input: path.clone(),
        requested: BTreeSet::from([Stage::Ir]),
        opt_level: OptLevel::O0,
    })
    .unwrap_or_else(|err| panic!("IR capture failed for {}: {}", path.display(), err));
    let elapsed = start.elapsed();

    match result.get(Stage::Ir) {
        Some(CapturedStage::Text(ir)) if ir.contains("func @") => {}
        Some(other) => panic!("unexpected IR capture for {}: {:?}", path.display(), other),
        None => panic!("missing IR capture for {}", path.display()),
    }

    let _ = fs::remove_file(path);
    elapsed
}

#[test]
fn intrinsic_operator_compile_time_scales_below_quadratic() {
    let warmup = temp_source("defop_warmup", 100);
    let _ = compile_ir(warmup);

    let small = compile_ir(temp_source("defop_small", 1000));
    let large = compile_ir(temp_source("defop_large", 2000));
    let ceiling = small.mul_f64(4.0) + Duration::from_millis(500);

    assert!(
        large < ceiling,
        "O0 compile time for 2000 intrinsic operator functions should stay below quadratic: \
         small={:?}, large={:?}, ceiling={:?}",
        small,
        large,
        ceiling
    );
}
