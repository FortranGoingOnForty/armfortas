use std::path::{Path, PathBuf};
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
    std::env::temp_dir().join(format!("afs_alloc_ctor_{}_{}_{}.{}", stem, pid, id, ext))
}

fn unique_dir(stem: &str) -> PathBuf {
    let dir = unique_path(stem, "dir");
    std::fs::create_dir_all(&dir).expect("cannot create allocate-constructor test directory");
    dir
}

fn write_program_in(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("cannot write allocate-constructor test source");
    path
}

fn compile_program(source: &Path, output: &Path) -> std::process::Output {
    Command::new(compiler("armfortas"))
        .args([source.to_str().unwrap(), "-o", output.to_str().unwrap()])
        .output()
        .expect("failed to spawn armfortas compile")
}

#[test]
fn allocate_source_array_constructor_infers_shape_and_copies_values() {
    let dir = unique_dir("source_array_ctor");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer, allocatable :: a(:)\n  allocate(a, source=[1, 2, 3])\n  if (.not. allocated(a)) error stop 1\n  if (size(a) /= 3) error stop 2\n  if (a(1) /= 1 .or. a(2) /= 2 .or. a(3) /= 3) error stop 3\n  print *, size(a)\n  print *, a(1), a(2), a(3)\nend program\n",
    );
    let exe = dir.join("source_array_ctor.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("source array constructor runtime failed");
    assert!(
        run.status.success(),
        "source array constructor runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("3")
            && stdout.contains("1")
            && stdout.contains("2")
            && stdout.contains("3"),
        "unexpected source array constructor output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocate_mold_array_constructor_infers_shape() {
    let dir = unique_dir("mold_array_ctor");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer, allocatable :: a(:)\n  allocate(a, mold=[1, 2, 3])\n  if (.not. allocated(a)) error stop 1\n  if (size(a) /= 3) error stop 2\n  print *, size(a)\nend program\n",
    );
    let exe = dir.join("mold_array_ctor.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("mold array constructor runtime failed");
    assert!(
        run.status.success(),
        "mold array constructor runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("3"),
        "unexpected mold array constructor output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocate_component_source_array_constructor_infers_shape_and_copies_values() {
    let dir = unique_dir("component_source_array_ctor");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  type :: box_t\n    integer, allocatable :: vals(:)\n  end type box_t\n  type(box_t) :: box\n  allocate(box%vals, source=[4, 5])\n  if (.not. allocated(box%vals)) error stop 1\n  if (size(box%vals) /= 2) error stop 2\n  if (box%vals(1) /= 4 .or. box%vals(2) /= 5) error stop 3\n  print *, size(box%vals)\n  print *, box%vals(1), box%vals(2)\nend program\n",
    );
    let exe = dir.join("component_source_array_ctor.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("component source array constructor runtime failed");
    assert!(
        run.status.success(),
        "component source array constructor runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("2") && stdout.contains("4") && stdout.contains("5"),
        "unexpected component source array constructor output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}
