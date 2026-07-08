use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn temp_source(name: &str, lines: usize) -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "afs_{}_{}_{}_{}.f90",
        name,
        std::process::id(),
        id,
        lines
    ));
    fs::write(&path, lsf_stress_source(lines)).expect("write generated source");
    path
}

fn lsf_stress_source(lines: usize) -> String {
    let bound = lines.next_power_of_two().max(16);
    let mut src = String::new();
    src.push_str("module lsf_stress_m\ncontains\n");
    src.push_str("  subroutine kernel(x)\n");
    src.push_str(&format!("    integer, intent(inout) :: x({})\n", bound));
    src.push_str("    x(1) = x(1) + 1\n");

    for k in 0..lines {
        let dst = k + 1;
        let lhs = (k * 17 + 3) % lines + 1;
        let rhs = (k * 31 + 7) % lines + 1;
        src.push_str(&format!(
            "    x({}) = mod(x({}) * 3 + x({}) + {}, 104729)\n",
            dst, lhs, rhs, k
        ));
    }

    src.push_str("  end subroutine kernel\nend module lsf_stress_m\n");
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
fn array_store_lsf_compile_time_scales_below_quadratic() {
    let warmup = temp_source("lsf_warmup", 32);
    let _ = compile_opt_ir(warmup);

    let small = compile_opt_ir(temp_source("lsf_small", 250));
    let large = compile_opt_ir(temp_source("lsf_large", 500));
    let ceiling = small.mul_f64(4.0) + Duration::from_secs(2);

    assert!(
        large < ceiling,
        "O2 compile time for 500 array-element stores should stay under a quadratic ceiling: \
         small={:?}, large={:?}, ceiling={:?}",
        small,
        large,
        ceiling
    );
}
