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
    std::env::temp_dir().join(format!("afs_memory_{}_{}_{}.{}", stem, pid, id, ext))
}

fn unique_dir(stem: &str) -> PathBuf {
    let dir = unique_path(stem, "dir");
    std::fs::create_dir_all(&dir).expect("cannot create memory-runtime test directory");
    dir
}

fn write_program_in(dir: &std::path::Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("cannot write memory-runtime test source");
    path
}

fn compile_program(source: &std::path::Path, output: &std::path::Path) -> std::process::Output {
    Command::new(compiler("armfortas"))
        .args([source.to_str().unwrap(), "-o", output.to_str().unwrap()])
        .output()
        .expect("failed to spawn armfortas compile")
}

#[test]
fn allocate_stat_errmsg_populates_fixed_character_target() {
    let dir = unique_dir("alloc_fixed_errmsg");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer :: ios\n  integer, allocatable :: a(:)\n  character(len=64) :: msg\n  msg = 'unchanged'\n  allocate(a(2), stat=ios, errmsg=msg)\n  if (ios /= 0) error stop 1\n  allocate(a(2), stat=ios, errmsg=msg)\n  if (ios == 0) error stop 2\n  if (index(trim(msg), 'ALLOCATE failed') == 0) error stop 3\n  print *, ios\n  print *, trim(msg)\nend program\n",
    );
    let exe = dir.join("alloc_fixed_errmsg.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("fixed errmsg runtime failed");
    assert!(
        run.status.success(),
        "fixed errmsg runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("2"),
        "expected nonzero STAT in fixed errmsg output: {}",
        stdout
    );
    assert!(
        stdout.contains("ALLOCATE failed"),
        "expected fixed errmsg text in output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocate_stat_errmsg_populates_deferred_character_target() {
    let dir = unique_dir("alloc_deferred_errmsg");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer :: ios\n  integer, allocatable :: a(:)\n  character(len=:), allocatable :: msg\n  msg = 'seed'\n  allocate(a(2), stat=ios, errmsg=msg)\n  if (ios /= 0) error stop 1\n  allocate(a(2), stat=ios, errmsg=msg)\n  if (ios == 0) error stop 2\n  if (.not. allocated(msg)) error stop 3\n  if (index(trim(msg), 'ALLOCATE failed') == 0) error stop 4\n  print *, ios\n  print *, trim(msg)\nend program\n",
    );
    let exe = dir.join("alloc_deferred_errmsg.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("deferred errmsg runtime failed");
    assert!(
        run.status.success(),
        "deferred errmsg runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("2"),
        "expected nonzero STAT in deferred errmsg output: {}",
        stdout
    );
    assert!(
        stdout.contains("ALLOCATE failed"),
        "expected deferred errmsg text in output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocate_errmsg_requires_scalar_character_target() {
    let dir = unique_dir("alloc_bad_errmsg");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer :: ios, msg\n  integer, allocatable :: a(:)\n  allocate(a(2), stat=ios, errmsg=msg)\nend program\n",
    );
    let exe = dir.join("alloc_bad_errmsg.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        !compile.status.success(),
        "compile unexpectedly succeeded:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );
    let stderr = String::from_utf8_lossy(&compile.stderr);
    assert!(
        stderr.contains("ERRMSG=") && stderr.contains("scalar CHARACTER variable"),
        "unexpected compile failure for bad ERRMSG target: {}",
        stderr
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deallocate_stat_errmsg_leaves_message_unchanged_on_success() {
    let dir = unique_dir("dealloc_fixed_errmsg");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer :: ios\n  integer, allocatable :: a(:)\n  character(len=64) :: msg\n  allocate(a(2))\n  msg = 'unchanged'\n  deallocate(a, stat=ios, errmsg=msg)\n  if (ios /= 0) error stop 1\n  if (trim(msg) /= 'unchanged') error stop 2\n  deallocate(a, stat=ios, errmsg=msg)\n  if (ios /= 0) error stop 3\n  if (trim(msg) /= 'unchanged') error stop 4\n  print *, ios\n  print *, trim(msg)\nend program\n",
    );
    let exe = dir.join("dealloc_fixed_errmsg.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("fixed deallocate errmsg runtime failed");
    assert!(
        run.status.success(),
        "fixed deallocate errmsg runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("0"),
        "expected zero STAT in fixed deallocate errmsg output: {}",
        stdout
    );
    assert!(
        stdout.contains("unchanged"),
        "expected unchanged errmsg text in output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}
