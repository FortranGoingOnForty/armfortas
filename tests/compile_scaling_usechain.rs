use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{Duration, Instant};

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn temp_source(name: &str, modules: usize) -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "afs_{}_{}_{}_{}.f90",
        name,
        std::process::id(),
        id,
        modules
    ));
    fs::write(&path, use_chain_source(modules)).expect("write generated source");
    path
}

fn use_chain_source(modules: usize) -> String {
    assert!(modules >= 2);

    let mut src = String::new();
    src.push_str("module mc000\n  implicit none\n  integer(8), parameter :: v000 = 1\ncontains\n");
    src.push_str(
        "  function f000(x) result(r)\n    integer(8), intent(in) :: x\n    integer(8) :: r\n",
    );
    src.push_str("    r = mod(x + v000, 1000000007_8)\n  end function f000\n");
    src.push_str("end module mc000\n");

    for idx in 1..modules {
        let prev = idx - 1;
        src.push_str(&format!(
            "module mc{idx:03}\n  use mc{prev:03}\n  implicit none\n  \
             integer(8), parameter :: v{idx:03} = v{prev:03} + {idx}\ncontains\n"
        ));
        src.push_str(&format!(
            "  function f{idx:03}(x) result(r)\n    integer(8), intent(in) :: x\n    \
             integer(8) :: r\n"
        ));
        src.push_str(&format!(
            "    r = mod(f{prev:03}(x) + v{idx:03}, 1000000007_8)\n  \
             end function f{idx:03}\nend module mc{idx:03}\n"
        ));
    }

    let last = modules - 1;
    src.push_str(&format!(
        "program chain\n  use mc{last:03}\n  implicit none\n  print '(a,i0)', 'chk=', \
         f{last:03}(777_8)\nend program chain\n"
    ));
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
fn linear_use_chain_compile_time_scales_below_quartic() {
    let warmup = temp_source("usechain_warmup", 20);
    let _ = compile_ir(warmup);

    let small = compile_ir(temp_source("usechain_small", 100));
    let large = compile_ir(temp_source("usechain_large", 200));
    let ceiling = small.mul_f64(4.0) + Duration::from_secs(2);

    assert!(
        large < ceiling,
        "O0 compile time for a 200-module USE chain should stay under a quadratic ceiling: \
         small={:?}, large={:?}, ceiling={:?}",
        small,
        large,
        ceiling
    );
}
