//! Cross-TU (multi-file) compilation tests.
//!
//! Each test compiles a module .f90 and a consumer .f90 separately
//! with `-c`, links the .o files with the runtime, runs the binary,
//! and checks the output.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn unique_dir() -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("afs_multifile_{}_{}", std::process::id(), id));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn find_compiler() -> PathBuf {
    for c in &["target/release/armfortas", "target/debug/armfortas"] {
        let p = PathBuf::from(c);
        if p.exists() {
            return std::fs::canonicalize(&p).unwrap();
        }
    }
    panic!("armfortas binary not found");
}

fn find_runtime() -> PathBuf {
    for dir in &["target/release", "target/debug"] {
        let p = PathBuf::from(dir).join("libarmfortas_rt.a");
        if p.exists() {
            return p;
        }
    }
    panic!("libarmfortas_rt.a not found");
}

fn sdk_path() -> String {
    let out = Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
        .expect("xcrun failed");
    String::from_utf8(out.stdout).unwrap().trim().to_string()
}

/// Compile a .f90 file with -c, producing .o and optionally .amod.
fn compile_file(compiler: &Path, source: &Path, output: &Path, search_dir: Option<&Path>) {
    let mut cmd = Command::new(compiler);
    if let Some(parent) = source.parent() {
        cmd.current_dir(parent);
    }
    cmd.args([
        source.to_str().unwrap(),
        "-c",
        "-o",
        output.to_str().unwrap(),
    ]);
    if let Some(dir) = search_dir {
        cmd.arg(format!("-I{}", dir.display()));
    }
    let result = cmd.output().expect("compiler launch failed");
    assert!(
        result.status.success(),
        "compile {} failed:\n{}",
        source.display(),
        String::from_utf8_lossy(&result.stderr)
    );
}

/// Link .o files into a binary.
fn link_files(objects: &[&Path], output: &Path) {
    let runtime = find_runtime();
    let sdk = sdk_path();
    let mut args: Vec<String> = vec!["-o".into(), output.to_str().unwrap().into()];
    for o in objects {
        args.push(o.to_str().unwrap().into());
    }
    args.push(runtime.to_str().unwrap().into());
    args.extend([
        "-lSystem".into(),
        "-syslibroot".into(),
        sdk,
        "-arch".into(),
        "arm64".into(),
    ]);
    let result = Command::new("ld")
        .args(&args)
        .output()
        .expect("ld launch failed");
    assert!(
        result.status.success(),
        "link failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

/// Run a binary and return its stdout.
fn run_binary(binary: &Path) -> String {
    let result = Command::new(binary).output().expect("binary launch failed");
    assert!(
        result.status.success(),
        "{} exited with {:?}\nstderr: {}",
        binary.display(),
        result.status.code(),
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8_lossy(&result.stdout).into_owned()
}

/// Full multi-file test: write sources, compile, link, run, check.
fn multifile_test(mod_source: &str, main_source: &str, expected_substring: &str) {
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("mod.f90");
    let main_f90 = dir.join("main.f90");
    let mod_o = dir.join("mod.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("test_bin");

    std::fs::write(&mod_f90, mod_source).unwrap();
    std::fs::write(&main_f90, main_source).unwrap();

    compile_file(&compiler, &mod_f90, &mod_o, None);
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&mod_o, &main_o], &binary);
    let output = run_binary(&binary);

    assert!(
        output.contains(expected_substring),
        "expected '{}' in output, got:\n{}",
        expected_substring,
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// ---- Tests ----

#[test]
fn basic_module_variable_and_subroutine() {
    multifile_test(
        "module m\n  implicit none\n  integer :: counter = 0\ncontains\n  subroutine bump()\n    counter = counter + 1\n  end subroutine\n  integer function get() result(r)\n    r = counter\n  end function\nend module\n",
        "program p\n  use m\n  call bump(); call bump(); call bump()\n  print *, get()\nend program\n",
        "3",
    );
}

#[test]
fn module_with_allocatable_array() {
    multifile_test(
        "module arr_mod\n  implicit none\n  integer, allocatable :: buf(:)\ncontains\n  subroutine init()\n    allocate(buf(3))\n    buf(1) = 10; buf(2) = 20; buf(3) = 30\n  end subroutine\nend module\n",
        "program p\n  use arr_mod\n  call init()\n  print *, buf(1), buf(2), buf(3)\nend program\n",
        "10 20 30",
    );
}

#[test]
fn module_with_derived_type() {
    multifile_test(
        "module dt_mod\n  implicit none\n  type :: point\n    real :: x, y\n  end type\ncontains\n  subroutine set_pt(p, a, b)\n    type(point), intent(out) :: p\n    real, intent(in) :: a, b\n    p%x = a; p%y = b\n  end subroutine\nend module\n",
        "program p\n  use dt_mod\n  type(point) :: pt\n  call set_pt(pt, 1.5, 2.5)\n  print *, pt%x, pt%y\nend program\n",
        "1.5",
    );
}

#[test]
fn module_parameter_constants() {
    multifile_test(
        "module consts\n  implicit none\n  integer, parameter :: MAX_N = 1024\n  integer, parameter :: HALF = MAX_N / 2\nend module\n",
        "program p\n  use consts\n  print *, MAX_N, HALF\nend program\n",
        "1024",
    );
}

#[test]
fn use_only_filtering() {
    multifile_test(
        "module big_mod\n  implicit none\n  integer :: alpha = 10\n  integer :: beta = 20\n  integer :: gamma = 30\nend module\n",
        "program p\n  use big_mod, only: beta\n  print *, beta\nend program\n",
        "20",
    );
}

#[test]
fn use_rename() {
    multifile_test(
        "module rename_mod\n  implicit none\n  integer :: original = 99\nend module\n",
        "program p\n  use rename_mod, renamed => original\n  print *, renamed\nend program\n",
        "99",
    );
}

/// Generic interface resolved across .amod boundaries: the consumer
/// reconstructs the NamedInterface from the @interface block and
/// dispatches each specific at the call site.
#[test]
fn generic_interface_cross_module() {
    multifile_test(
        "module mgen\n  implicit none\n  interface add\n    module procedure add_int, add_real\n  end interface\ncontains\n  integer function add_int(a, b)\n    integer, intent(in) :: a, b\n    add_int = a + b\n  end function\n  real function add_real(a, b)\n    real, intent(in) :: a, b\n    add_real = a + b\n  end function\nend module\n",
        "program p\n  use mgen\n  print *, add(1, 2)\n  print *, add(1.5, 2.5)\nend program\n",
        "3",
    );
}

/// Generic interface reachable transitively through an intermediate
/// module that re-exports via `USE`. The middle module's .amod has
/// only `@uses base`; the consumer must recursively load base and
/// re-expose its symbols (including the NamedInterface) so generic
/// dispatch walks the chain.
#[test]
fn generic_interface_transitive_use() {
    let compiler = find_compiler();
    let dir = unique_dir();
    let base_f90 = dir.join("base.f90");
    let middle_f90 = dir.join("middle.f90");
    let main_f90 = dir.join("main.f90");
    let base_o = dir.join("base.o");
    let middle_o = dir.join("middle.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("test_bin");

    std::fs::write(&base_f90, "module base\n  implicit none\n  interface add\n    module procedure add_int, add_real\n  end interface\ncontains\n  integer function add_int(a, b)\n    integer, intent(in) :: a, b\n    add_int = a + b\n  end function\n  real function add_real(a, b)\n    real, intent(in) :: a, b\n    add_real = a + b\n  end function\nend module\n").unwrap();
    std::fs::write(&middle_f90, "module middle\n  use base\nend module\n").unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use middle\n  print *, add(1, 2)\n  print *, add(1.5, 2.5)\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &base_f90, &base_o, None);
    compile_file(&compiler, &middle_f90, &middle_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &middle_o, &base_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("3"),
        "expected '3' in output, got:\n{}",
        output
    );
    assert!(
        output.contains("4.0000000E0"),
        "expected real add result in output, got:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn module_private_default() {
    // priv_val should not be accessible; only pub_val.
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("mod.f90");
    let mod_o = dir.join("mod.o");

    std::fs::write(&mod_f90,
        "module priv_mod\n  implicit none\n  private\n  integer, public :: pub_val = 42\n  integer :: priv_val = 99\nend module\n"
    ).unwrap();
    compile_file(&compiler, &mod_f90, &mod_o, None);

    // Check .amod only has pub_val.
    let amod = std::fs::read_to_string(dir.join("priv_mod.amod")).unwrap();
    assert!(amod.contains("pub_val"), "pub_val should be in .amod");
    assert!(
        !amod.contains("priv_val"),
        "priv_val should NOT be in .amod"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
