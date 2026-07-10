use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::thread;
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

fn compiler() -> PathBuf {
    option_env!("CARGO_BIN_EXE_armfortas")
        .map(PathBuf::from)
        .unwrap_or_else(|| {
            let mut p = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
            p.push("target/debug/armfortas");
            p
        })
}

fn unique_dir(stem: &str) -> PathBuf {
    let mut dir = std::env::temp_dir();
    dir.push(format!(
        "armfortas_compile_scaling_lsf_{}_{}_{}",
        stem,
        std::process::id(),
        SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .expect("system time before epoch")
            .as_nanos()
    ));
    fs::create_dir_all(&dir).expect("create scaling test dir");
    dir
}

fn lsf_source(assignments: usize) -> String {
    let extent = (assignments * 4).next_power_of_two().max(1024);
    let mut src = String::new();
    src.push_str("subroutine kernel(x)\n");
    src.push_str("  implicit none\n");
    src.push_str(&format!("  integer, intent(inout) :: x({extent})\n"));
    src.push_str("  integer :: k\n");
    src.push_str("  x = 1\n");
    for i in 1..=assignments {
        let a = (i % extent) + 1;
        let b = ((i * 7) % extent) + 1;
        let c = ((i * 13) % extent) + 1;
        src.push_str(&format!(
            "  x({a}) = mod(x({b}) * 3 + x({c}) + {i}, 1000003)\n"
        ));
    }
    src.push_str("  k = x(1)\n");
    src.push_str("  if (k == -1) print *, k\n");
    src.push_str("end subroutine kernel\n");
    src
}

fn compile_timed(compiler: &Path, src: &Path, asm: &Path, timeout: Duration) -> Duration {
    let start = Instant::now();
    let mut child = Command::new(compiler)
        .arg(src)
        .args(["-O2", "-S", "-o"])
        .arg(asm)
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()
        .expect("spawn armfortas");

    loop {
        if let Some(status) = child.try_wait().expect("poll armfortas") {
            assert!(status.success(), "armfortas compile failed with {status}");
            return start.elapsed();
        }
        if start.elapsed() > timeout {
            let _ = child.kill();
            let _ = child.wait();
            panic!(
                "armfortas compile exceeded {timeout:?} for {}",
                src.display()
            );
        }
        thread::sleep(Duration::from_millis(20));
    }
}

fn measure(assignments: usize) -> Duration {
    let dir = unique_dir(&assignments.to_string());
    let src = dir.join("lsf.f90");
    let asm = dir.join("lsf.s");
    fs::write(&src, lsf_source(assignments)).expect("write scaling source");
    let elapsed = compile_timed(&compiler(), &src, &asm, Duration::from_secs(30));
    let _ = fs::remove_dir_all(dir);
    elapsed
}

#[test]
fn array_element_store_forwarding_stays_below_quadratic_ceiling() {
    let small = measure(250);
    let large = measure(500);
    let ceiling = small.mul_f64(4.0) + Duration::from_secs(2);
    assert!(
        large < ceiling,
        "LSF compile-time growth exceeded ceiling: 250={small:?}, 500={large:?}, ceiling={ceiling:?}"
    );
}
