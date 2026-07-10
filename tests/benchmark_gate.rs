use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

const PROGRAMS: &[&str] = &[
    "array_bulk_kernels.f90",
    "module_init.f90",
    "two_loops.f90",
    "derived_type_nested.f90",
    "allocatable.f90",
    "ar6_bss_module_data.f90",
];

fn scratch_dir() -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "armfortas-benchmark-gate-{}-{id}",
        std::process::id()
    ));
    fs::create_dir_all(dir.join("test_programs")).expect("create benchmark scratch tree");
    dir
}

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/benchmark_gate.sh")
}

#[test]
fn missing_fixed_fixture_fails_before_benchmarking() {
    let root = scratch_dir();
    let missing = "two_loops.f90";
    for fixture in PROGRAMS {
        if *fixture != missing {
            fs::write(
                root.join("test_programs").join(fixture),
                b"program p\nend\n",
            )
            .expect("write benchmark fixture");
        }
    }

    let output = Command::new("bash")
        .arg(script())
        .current_dir(&root)
        .output()
        .expect("run benchmark gate");
    let _ = fs::remove_dir_all(&root);

    assert!(!output.status.success(), "missing fixture must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("missing mandatory benchmark fixture"));
    assert!(stderr.contains(missing));
}
