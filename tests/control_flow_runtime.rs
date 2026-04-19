use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

fn compiler(name: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(format!("CARGO_BIN_EXE_{}", name)) {
        return PathBuf::from(path);
    }
    let candidate = PathBuf::from("target/debug").join(name);
    if candidate.exists() {
        return std::fs::canonicalize(candidate).expect("cannot canonicalize debug compiler path");
    }
    let candidate = PathBuf::from("target/release").join(name);
    if candidate.exists() {
        return std::fs::canonicalize(candidate)
            .expect("cannot canonicalize release compiler path");
    }
    panic!(
        "compiler binary '{}' not built — run `cargo build --bins` first",
        name
    );
}

fn unique_path(stem: &str, ext: &str) -> PathBuf {
    let pid = std::process::id();
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("afs_ctrl_{}_{}_{}.{}", stem, pid, id, ext))
}

fn write_program(text: &str, suffix: &str) -> PathBuf {
    let path = unique_path("src", suffix);
    std::fs::write(&path, text).expect("cannot write control-flow test source");
    path
}

#[test]
fn named_exit_and_cycle_target_nested_constructs() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: i, j, sum\n  sum = 0\nouter: do i = 1, 4\n  inner: do j = 1, 4\n    if (j == 2) cycle inner\n    if (i == 3 .and. j == 4) exit outer\n    sum = sum + i * 10 + j\n  end do inner\nend do outer\nprint *, sum\nend program\n",
        "f90",
    );
    let out = unique_path("named_exit_cycle", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("named EXIT/CYCLE compile failed to spawn");
    assert!(
        compile.status.success(),
        "named EXIT/CYCLE compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("named EXIT/CYCLE run failed");
    assert!(
        run.status.success(),
        "named EXIT/CYCLE run failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("170"),
        "unexpected named EXIT/CYCLE output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}
