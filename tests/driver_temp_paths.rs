use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

fn compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary not built for this test profile")
}

fn unique_dir(stem: &str) -> PathBuf {
    let pid = std::process::id();
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("afs_{}_{}_{}.dir", stem, pid, id));
    std::fs::create_dir_all(&dir).expect("cannot create temp dir");
    dir
}

fn run_compile(compiler: &Path, dir: &Path) -> Output {
    Command::new(compiler)
        .current_dir(dir)
        .args(["p.f90", "-o", "t"])
        .output()
        .expect("compiler launch failed")
}

#[test]
fn parallel_relative_outputs_with_same_basename_keep_separate_temps() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_temp_paths test=parallel_relative_outputs_with_same_basename_keep_separate_temps count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let compiler = compiler();
    let root = unique_dir("parallel_same_output_basename");
    let cases: Vec<(PathBuf, String)> = (0..8)
        .map(|i| {
            let dir = root.join(format!("case_{i}"));
            std::fs::create_dir_all(&dir).expect("cannot create case dir");
            let marker = format!("case_marker_{i}");
            std::fs::write(
                dir.join("p.f90"),
                format!("program p\n  print *, '{}'\nend program\n", marker),
            )
            .expect("cannot write test source");
            (dir, marker)
        })
        .collect();

    let handles: Vec<_> = cases
        .iter()
        .map(|(dir, _)| {
            let compiler = compiler.clone();
            let dir = dir.clone();
            std::thread::spawn(move || run_compile(&compiler, &dir))
        })
        .collect();

    for (idx, handle) in handles.into_iter().enumerate() {
        let output = handle.join().expect("compile thread panicked");
        assert!(
            output.status.success(),
            "compile {idx} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    for (dir, marker) in &cases {
        let binary = dir.join("t");
        let output = Command::new(&binary)
            .output()
            .expect("compiled binary launch failed");
        assert!(
            output.status.success(),
            "{} exited with {:?}\nstderr: {}",
            binary.display(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(marker),
            "{} printed wrong output:\n{}",
            binary.display(),
            stdout
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}
