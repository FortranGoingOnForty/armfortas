use std::path::PathBuf;
use std::process::{Command, Output, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::thread::sleep;
use std::time::{Duration, Instant};

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

fn run_with_timeout(path: &std::path::Path) -> Output {
    let mut child = Command::new(path)
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("failed to spawn control-flow test binary");
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        if let Some(_status) = child.try_wait().expect("failed to poll child status") {
            return child
                .wait_with_output()
                .expect("failed to collect child output");
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("control-flow test binary hung: {}", path.display());
        }
        sleep(Duration::from_millis(20));
    }
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

#[test]
fn character_select_case_matches_expected_arm() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: code\n  character(len=8) :: cmd\n  cmd = 'help'\n  code = dispatch(cmd)\n  if (code /= 2) error stop 1\n  cmd = 'exit'\n  code = dispatch(cmd)\n  if (code /= 1) error stop 2\n  cmd = 'other'\n  code = dispatch(cmd)\n  if (code /= 3) error stop 3\n  print *, 99\ncontains\n  integer function dispatch(cmd) result(code)\n    character(len=*), intent(in) :: cmd\n    select case (trim(cmd))\n    case ('quit', 'exit')\n      code = 1\n    case ('help')\n      code = 2\n    case default\n      code = 3\n    end select\n  end function dispatch\nend program\n",
        "f90",
    );
    let out = unique_path("char_select_case", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("character SELECT CASE compile failed to spawn");
    assert!(
        compile.status.success(),
        "character SELECT CASE compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("character SELECT CASE run failed");
    assert!(
        run.status.success(),
        "character SELECT CASE run failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("99"),
        "unexpected character SELECT CASE output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn logical_and_or_short_circuit_in_expression_values() {
    let src = write_program(
        "program p\n  implicit none\n  logical :: x, y\n  x = .false. .and. boom()\n  y = .true. .or. boom()\n  if (x) error stop 1\n  if (.not. y) error stop 2\n  print *, 77\ncontains\n  logical function boom()\n    error stop 7\n  end function boom\nend program\n",
        "f90",
    );
    let out = unique_path("short_circuit_expr", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("expression short-circuit compile failed to spawn");
    assert!(
        compile.status.success(),
        "expression short-circuit compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("expression short-circuit run failed");
    assert!(
        run.status.success(),
        "expression short-circuit run failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("77"),
        "unexpected expression short-circuit output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn runtime_zero_step_do_fails_loudly_instead_of_hanging() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: i, step\n  step = 0\n  do i = 1, 10, step\n    print *, i\n  end do\n  print *, 88\nend program\n",
        "f90",
    );
    let out = unique_path("runtime_zero_step", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("runtime zero-step compile failed to spawn");
    assert!(
        compile.status.success(),
        "runtime zero-step compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = run_with_timeout(&out);
    assert!(
        !run.status.success(),
        "runtime zero-step DO should fail instead of succeeding"
    );
    let stderr = String::from_utf8_lossy(&run.stderr);
    assert!(
        stderr.contains("ERROR STOP"),
        "expected runtime zero-step diagnostic, got stderr: {}",
        stderr
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn case_default_runs_only_after_other_cases_fail() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: x\n  x = 2\n  select case (x)\n  case default\n    print *, 0\n  case (2)\n    print *, 2\n  end select\nend program\n",
        "f90",
    );
    let out = unique_path("select_default_fallback", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("select default fallback compile failed to spawn");
    assert!(
        compile.status.success(),
        "select default fallback compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("select default fallback run failed");
    assert!(
        run.status.success(),
        "select default fallback run failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("2"),
        "default arm should not swallow later matching case: {}",
        stdout
    );
    assert!(
        !stdout.contains("0"),
        "default arm should only run as fallback: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}
