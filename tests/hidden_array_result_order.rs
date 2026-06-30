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
    panic!(
        "compiler binary '{}' not built - run `cargo build --bins` first",
        name
    );
}

fn unique_path(stem: &str, ext: &str) -> PathBuf {
    let pid = std::process::id();
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("afs_hidden_result_{}_{}_{}.{}", stem, pid, id, ext))
}

fn write_program(text: &str) -> PathBuf {
    let path = unique_path("src", "f90");
    std::fs::write(&path, text).expect("cannot write test source");
    path
}

#[test]
fn automatic_local_can_depend_on_hidden_array_result_shape() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=hidden_array_result_order test=automatic_local_can_depend_on_hidden_array_result_shape count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let src = write_program(
        r#"
module m
  implicit none
contains
  function make(n) result(d)
    integer, intent(in) :: n
    real(8) :: d(n)
    real(8) :: z(size(d), size(d))

    z = 0.0_8
    if (size(z, 1) /= n .or. size(z, 2) /= n) error stop 1
    d = 1.0_8
  end function
end module

program p
  use m, only : make
  implicit none
  real(8), allocatable :: got(:)

  got = make(2)
  if (size(got) /= 2) error stop 2
  print *, 'ok'
end program
"#,
    );
    let out = unique_path("bin", "exe");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("compile failed to spawn");
    assert!(
        compile.status.success(),
        "hidden result shape before locals should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed to spawn");
    assert!(
        run.status.success(),
        "hidden result shape before locals should run: status={:?} stderr={} stdout={}",
        run.status,
        String::from_utf8_lossy(&run.stderr),
        String::from_utf8_lossy(&run.stdout)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}
