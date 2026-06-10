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
    std::env::temp_dir().join(format!(
        "afs_alloc_validate_{}_{}_{}.{}",
        stem, pid, id, ext
    ))
}

fn unique_dir(stem: &str) -> PathBuf {
    let dir = unique_path(stem, "dir");
    std::fs::create_dir_all(&dir).expect("cannot create allocate-validation test directory");
    dir
}

fn write_program_in(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("cannot write allocate-validation test source");
    path
}

fn compile_program(source: &Path, output: &Path) -> std::process::Output {
    Command::new(compiler("armfortas"))
        .args([source.to_str().unwrap(), "-o", output.to_str().unwrap()])
        .output()
        .expect("failed to spawn armfortas compile")
}

fn compile_with_args(args: &[&str]) -> std::process::Output {
    Command::new(compiler("armfortas"))
        .args(args)
        .output()
        .expect("failed to spawn armfortas compile")
}

#[test]
fn bare_allocate_array_requires_shape_or_source_or_mold() {
    let dir = unique_dir("bare_array");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer, allocatable :: a(:)\n  allocate(a)\nend program\n",
    );
    let exe = dir.join("bare_array.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        !compile.status.success(),
        "compile unexpectedly succeeded:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );
    let stderr = String::from_utf8_lossy(&compile.stderr);
    assert!(
        stderr.contains("array ALLOCATE requires bounds or SOURCE=/MOLD="),
        "unexpected compile failure for bare array allocate: {}",
        stderr
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bare_allocate_component_array_requires_shape_or_source_or_mold() {
    let dir = unique_dir("bare_component_array");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  type :: box_t\n    integer, allocatable :: vals(:)\n  end type box_t\n  type(box_t) :: box\n  allocate(box%vals)\nend program\n",
    );
    let exe = dir.join("bare_component_array.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        !compile.status.success(),
        "compile unexpectedly succeeded:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );
    let stderr = String::from_utf8_lossy(&compile.stderr);
    assert!(
        stderr.contains("array ALLOCATE requires bounds or SOURCE=/MOLD="),
        "unexpected compile failure for bare component array allocate: {}",
        stderr
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bare_allocate_scalar_allocatable_still_works() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=allocate_validation test=bare_allocate_scalar_allocatable_still_works count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("bare_scalar");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer, allocatable :: x\n  allocate(x)\n  x = 7\n  print *, allocated(x)\n  print *, x\nend program\n",
    );
    let exe = dir.join("bare_scalar.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("scalar allocate runtime failed");
    assert!(
        run.status.success(),
        "scalar allocate runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("T") && stdout.contains("7"),
        "expected allocated scalar output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_allocatable_component_array_requires_shape_or_source_or_mold() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=allocate_validation test=imported_allocatable_component_array_requires_shape_or_source_or_mold count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("imported_component_array");
    let mod_src = write_program_in(
        &dir,
        "m.f90",
        "module m\n  implicit none\n  type :: box_t\n    integer, allocatable :: vals(:)\n  end type box_t\nend module\n",
    );
    let mod_obj = dir.join("m.o");
    let compile_mod = compile_with_args(&[
        "-c",
        mod_src.to_str().unwrap(),
        "-J",
        dir.to_str().unwrap(),
        "-o",
        mod_obj.to_str().unwrap(),
    ]);
    assert!(
        compile_mod.status.success(),
        "module compile failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile_mod.stdout),
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use m, only: box_t\n  implicit none\n  type(box_t) :: box\n  allocate(box%vals)\nend program\n",
    );
    let exe = dir.join("imported_component_array.bin");
    let compile_main = compile_with_args(&[
        main_src.to_str().unwrap(),
        "-I",
        dir.to_str().unwrap(),
        "-o",
        exe.to_str().unwrap(),
    ]);
    assert!(
        !compile_main.status.success(),
        "compile unexpectedly succeeded:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile_main.stdout),
        String::from_utf8_lossy(&compile_main.stderr)
    );
    let stderr = String::from_utf8_lossy(&compile_main.stderr);
    assert!(
        stderr.contains("array ALLOCATE requires bounds or SOURCE=/MOLD="),
        "unexpected compile failure for imported component array allocate: {}",
        stderr
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_allocatable_component_scalar_still_works() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=allocate_validation test=imported_allocatable_component_scalar_still_works count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("imported_component_scalar");
    let mod_src = write_program_in(
        &dir,
        "m.f90",
        "module m\n  implicit none\n  type :: box_t\n    integer, allocatable :: val\n  end type box_t\nend module\n",
    );
    let mod_obj = dir.join("m.o");
    let compile_mod = compile_with_args(&[
        "-c",
        mod_src.to_str().unwrap(),
        "-J",
        dir.to_str().unwrap(),
        "-o",
        mod_obj.to_str().unwrap(),
    ]);
    assert!(
        compile_mod.status.success(),
        "module compile failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile_mod.stdout),
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use m, only: box_t\n  implicit none\n  type(box_t) :: box\n  allocate(box%val)\n  box%val = 9\n  print *, box%val\nend program\n",
    );
    let exe = dir.join("imported_component_scalar.bin");
    let compile_main = compile_with_args(&[
        main_src.to_str().unwrap(),
        "-I",
        dir.to_str().unwrap(),
        "-o",
        exe.to_str().unwrap(),
    ]);
    assert!(
        compile_main.status.success(),
        "compile failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile_main.stdout),
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("imported scalar allocate runtime failed");
    assert!(
        run.status.success(),
        "imported scalar allocate runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("9"),
        "expected imported scalar allocate output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}
