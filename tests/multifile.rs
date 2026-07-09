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
    // CARGO_BIN_EXE points at the binary built for THIS test profile;
    // the path probes below can hit a stale binary from the other
    // profile (a release armfortas predating module-global emission
    // failed every multifile test while the debug build was fine).
    if let Some(p) = std::env::var_os("CARGO_BIN_EXE_armfortas") {
        return PathBuf::from(p);
    }
    for c in &["target/release/armfortas", "target/debug/armfortas"] {
        let p = PathBuf::from(c);
        if p.exists() {
            return std::fs::canonicalize(&p).unwrap();
        }
    }
    panic!("armfortas binary not found");
}

fn find_runtime() -> PathBuf {
    // Mirror find_compiler's staleness fix: prefer THIS test
    // profile's archive. With a stale other-profile runtime first,
    // the linker pulls members from both archives and errors on
    // duplicate symbols the moment the fresh runtime gains a symbol
    // the stale one lacks.
    let dirs: [&str; 2] = if cfg!(debug_assertions) {
        ["target/debug", "target/release"]
    } else {
        ["target/release", "target/debug"]
    };
    for dir in &dirs {
        let p = PathBuf::from(dir).join("libarmfortas_rt.a");
        if p.exists() {
            return p;
        }
    }
    panic!("libarmfortas_rt.a not found");
}

/// Compile a .f90 file with -c, producing .o and optionally .amod.
fn compile_file(compiler: &Path, source: &Path, output: &Path, search_dir: Option<&Path>) {
    compile_file_flags(compiler, source, output, search_dir, &[]);
}

fn compile_file_flags(
    compiler: &Path,
    source: &Path,
    output: &Path,
    search_dir: Option<&Path>,
    flags: &[&str],
) {
    let mut cmd = Command::new(compiler);
    if let Some(parent) = source.parent() {
        cmd.current_dir(parent);
    }
    cmd.args(flags);
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
    // Link through the compiler binary: the driver owns crt discovery,
    // runtime location, and the per-format link line on every platform
    // (the old inline ld invocation was Mach-O-only).
    let compiler = find_compiler();
    let runtime = find_runtime();
    let mut cmd = Command::new(&compiler);
    for o in objects {
        cmd.arg(o);
    }
    let result = cmd
        .arg(&runtime)
        .arg("-o")
        .arg(output)
        .output()
        .expect("compiler launch failed for link");
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

fn undefined_symbols(path: &Path) -> Vec<String> {
    let out = Command::new("nm")
        .args(["-u", "-j", path.to_str().unwrap()])
        .output()
        .expect("failed to spawn nm");
    assert!(
        out.status.success(),
        "nm failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Full multi-file test: write sources, compile, link, run, check.
fn multifile_test(mod_source: &str, main_source: &str, expected_substring: &str) {
    multifile_test_flags(mod_source, main_source, expected_substring, &[]);
}

fn multifile_test_flags(
    mod_source: &str,
    main_source: &str,
    expected_substring: &str,
    flags: &[&str],
) {
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("mod.f90");
    let main_f90 = dir.join("main.f90");
    let mod_o = dir.join("mod.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("test_bin");

    std::fs::write(&mod_f90, mod_source).unwrap();
    std::fs::write(&main_f90, main_source).unwrap();

    compile_file_flags(&compiler, &mod_f90, &mod_o, None, flags);
    compile_file_flags(&compiler, &main_f90, &main_o, Some(&dir), flags);
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
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=basic_module_variable_and_subroutine count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    multifile_test(
        "module m\n  implicit none\n  integer :: counter = 0\ncontains\n  subroutine bump()\n    counter = counter + 1\n  end subroutine\n  integer function get() result(r)\n    r = counter\n  end function\nend module\n",
        "program p\n  use m\n  call bump(); call bump(); call bump()\n  print *, get()\nend program\n",
        "3",
    );
}

#[test]
fn module_with_allocatable_array() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=module_with_allocatable_array count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    multifile_test(
        "module arr_mod\n  implicit none\n  integer, allocatable :: buf(:)\ncontains\n  subroutine init()\n    allocate(buf(3))\n    buf(1) = 10; buf(2) = 20; buf(3) = 30\n  end subroutine\nend module\n",
        "program p\n  use arr_mod\n  call init()\n  print *, buf(1), buf(2), buf(3)\nend program\n",
        "10 20 30",
    );
}

// Regression: gfortran/flang accept Fortran sources and prebuilt objects
// mixed on one command line, e.g. `fc main.f90 mod.o -o prog`. fortsh's
// unit-test rules use exactly this shape (`fc test.f90 build/foo.o -o test`).
// armfortas used to reject it ("mixing Fortran sources with prebuilt
// object/archive inputs is not yet supported"); now it compiles the sources
// and links them with the artifacts in command order.
#[test]
fn mixed_source_and_object_in_one_invocation() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=mixed_source_and_object_in_one_invocation count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("mixmod.f90");
    let main_f90 = dir.join("mixmain.f90");
    let mod_o = dir.join("mixmod.o");
    let binary = dir.join("mixbin");

    std::fs::write(
        &mod_f90,
        "module mixmod\n  implicit none\ncontains\n  integer function answer() result(r)\n    r = 42\n  end function\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use mixmod\n  print *, answer()\nend program\n",
    )
    .unwrap();

    // Compile the module to an object up front.
    compile_file(&compiler, &mod_f90, &mod_o, None);

    // The fix under test: one invocation with a SOURCE and an OBJECT.
    let result = Command::new(&compiler)
        .arg(&main_f90)
        .arg(&mod_o)
        .arg("-o")
        .arg(&binary)
        .arg(format!("-I{}", dir.display()))
        .output()
        .expect("compiler launch failed for mixed source+object");
    assert!(
        result.status.success(),
        "mixed source+object invocation failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let output = run_binary(&binary);
    assert!(
        output.contains("42"),
        "expected '42' in output, got:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn truncated_amod_is_rejected_loudly() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=truncated_amod_is_rejected_loudly count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let provider_f90 = dir.join("provider.f90");
    let provider_o = dir.join("provider.o");
    let consumer_f90 = dir.join("consumer.f90");
    let consumer_o = dir.join("consumer.o");

    std::fs::write(
        &provider_f90,
        "module provider\n  implicit none\n  integer, parameter :: answer = 41\nend module\n",
    )
    .unwrap();
    compile_file(&compiler, &provider_f90, &provider_o, None);

    let amod_path = dir.join("provider.amod");
    let mut amod_text = std::fs::read_to_string(&amod_path).expect("missing provider.amod");
    let truncate_at = amod_text
        .find("@param answer")
        .expect("provider.amod should contain answer parameter");
    amod_text.truncate(truncate_at);
    std::fs::write(&amod_path, amod_text).expect("cannot corrupt provider.amod");

    std::fs::write(
        &consumer_f90,
        "program p\n  use provider\n  implicit none\n  print *, answer\nend program\n",
    )
    .unwrap();
    let result = Command::new(&compiler)
        .current_dir(&dir)
        .arg(&consumer_f90)
        .arg("-c")
        .arg("-o")
        .arg(&consumer_o)
        .arg(format!("-I{}", dir.display()))
        .output()
        .expect("consumer compile failed to spawn");
    assert!(
        !result.status.success(),
        "consumer unexpectedly accepted a corrupt .amod"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("corrupt .amod file"),
        "expected corrupt .amod diagnostic, got:\n{}",
        stderr
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn amod_omits_stale_abi_stamp() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=amod_omits_stale_abi_stamp count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let module_f90 = dir.join("target_stamp.f90");
    let module_o = dir.join("target_stamp.o");

    std::fs::write(
        &module_f90,
        "module target_stamp\n  implicit none\n  integer, parameter :: answer = 42\nend module\n",
    )
    .unwrap();
    compile_file(&compiler, &module_f90, &module_o, None);

    let amod =
        std::fs::read_to_string(dir.join("target_stamp.amod")).expect("missing target_stamp.amod");
    assert!(
        !amod.lines().any(|line| line.starts_with("# abi:")),
        ".amod should not stamp a non-authoritative ABI line:\n{}",
        amod
    );
    assert!(
        !amod.contains("cc=aapcs64") && !amod.contains("@abi pass="),
        ".amod should not stamp target-specific procedure ABI annotations:\n{}",
        amod
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn module_with_derived_type() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=module_with_derived_type count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    multifile_test(
        "module dt_mod\n  implicit none\n  type :: point\n    real :: x, y\n  end type\ncontains\n  subroutine set_pt(p, a, b)\n    type(point), intent(out) :: p\n    real, intent(in) :: a, b\n    p%x = a; p%y = b\n  end subroutine\nend module\n",
        "program p\n  use dt_mod\n  type(point) :: pt\n  call set_pt(pt, 1.5, 2.5)\n  print *, pt%x, pt%y\nend program\n",
        "1.5",
    );
}

#[test]
fn module_parameter_constants() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=module_parameter_constants count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    multifile_test(
        "module consts\n  implicit none\n  integer, parameter :: MAX_N = 1024\n  integer, parameter :: HALF = MAX_N / 2\nend module\n",
        "program p\n  use consts\n  print *, MAX_N, HALF\nend program\n",
        "1024",
    );
}

#[test]
fn use_only_filtering() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=use_only_filtering count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    multifile_test(
        "module big_mod\n  implicit none\n  integer :: alpha = 10\n  integer :: beta = 20\n  integer :: gamma = 30\nend module\n",
        "program p\n  use big_mod, only: beta\n  print *, beta\nend program\n",
        "20",
    );
}

#[test]
fn use_only_excludes_defined_assignment_from_amod() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=use_only_excludes_defined_assignment_from_amod count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let t_f90 = dir.join("t.f90");
    let e_f90 = dir.join("e.f90");
    let main_f90 = dir.join("main.f90");
    let t_o = dir.join("t.o");
    let e_o = dir.join("e.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("test_bin");

    std::fs::write(
        &t_f90,
        r#"module t
  implicit none
  type :: v
    integer, allocatable :: a(:)
  end type
  interface assignment(=)
    module procedure asn
  end interface
contains
  function mk() result(r)
    type(v) :: r
    allocate(r%a(1))
    r%a(1) = 9
  end function

  subroutine asn(lhs, rhs)
    type(v), intent(out) :: lhs
    type(v), intent(in) :: rhs
    if (allocated(rhs%a)) print '(a)', 'defined assignment fired'
  end subroutine
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &e_f90,
        r#"module e
  use t, only: v, mk
  implicit none
contains
  function go() result(r)
    type(v) :: r
    r = mk()
  end function
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        r#"program p
  use t, only: v
  use e, only: go
  implicit none
  type(v) :: x
  x = go()
  print '(a,l1)', 'alloc=', allocated(x%a)
  if (.not. allocated(x%a)) stop 1
end program
"#,
    )
    .unwrap();

    compile_file(&compiler, &t_f90, &t_o, None);
    compile_file(&compiler, &e_f90, &e_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&t_o, &e_o, &main_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("alloc=T"),
        "expected intrinsic assignment to preserve allocatable component, got:\n{}",
        output
    );
    assert!(
        !output.contains("defined assignment fired"),
        "defined assignment leaked through USE ONLY:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn use_rename() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=use_rename count=1 reason=\"{}\"",
            reason
        );
        return;
    }
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
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=generic_interface_cross_module count=1 reason=\"{}\"",
            reason
        );
        return;
    }
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
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=generic_interface_transitive_use count=1 reason=\"{}\"",
            reason
        );
        return;
    }
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
fn submodule_host_association_resolves_transitive_real_parameter() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=submodule_host_association_resolves_transitive_real_parameter count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let consts_f90 = dir.join("consts.f90");
    let middle_f90 = dir.join("middle.f90");
    let parent_f90 = dir.join("parent.f90");
    let body_f90 = dir.join("body.f90");
    let main_f90 = dir.join("main.f90");
    let consts_o = dir.join("consts.o");
    let middle_o = dir.join("middle.o");
    let parent_o = dir.join("parent.o");
    let body_o = dir.join("body.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("test_bin");

    std::fs::write(
        &consts_f90,
        "module consts_m\n  implicit none\n  public\n  real, parameter :: one = 1.0\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &middle_f90,
        "module middle_m\n  use consts_m\n  implicit none\n  public\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &parent_f90,
        "module parent_m\n  use middle_m\n  implicit none\n  private\n  interface\n    module subroutine fill(y)\n      real, intent(out) :: y\n    end subroutine\n  end interface\n  public :: fill\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &body_f90,
        "submodule(parent_m) parent_body\ncontains\n  module subroutine fill(y)\n    real, intent(out) :: y\n    y = one\n  end subroutine\nend submodule\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use parent_m, only: fill\n  implicit none\n  real :: y\n  call fill(y)\n  if (abs(y - 1.0) > 0.001) error stop 10\n  print *, 'ok'\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &consts_f90, &consts_o, None);
    compile_file(&compiler, &middle_f90, &middle_o, Some(&dir));
    compile_file(&compiler, &parent_f90, &parent_o, Some(&dir));
    compile_file(&compiler, &body_f90, &body_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(
        &[&main_o, &body_o, &parent_o, &middle_o, &consts_o],
        &binary,
    );
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "expected transitive parameter submodule body to print ok, got:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn generic_interface_beats_private_renamed_import() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=generic_interface_beats_private_renamed_import count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let dep_f90 = dir.join("dep.f90");
    let wrapper_f90 = dir.join("wrapper.f90");
    let main_f90 = dir.join("main.f90");
    let dep_o = dir.join("dep.o");
    let wrapper_o = dir.join("wrapper.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("test_bin");

    std::fs::write(
        &dep_f90,
        "module dep\n  implicit none\ncontains\n  integer function pick(x)\n    integer, intent(in) :: x\n    pick = -1\n  end function\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &wrapper_f90,
        "module wrapper\n  use dep, only: pick_dep => pick\n  implicit none\n  private\n  public :: box, pick\n  type :: box\n    integer :: v\n  end type\n  interface pick\n    module procedure pick_box\n  end interface\ncontains\n  integer function pick_box(x)\n    type(box), intent(in) :: x\n    pick_box = x%v\n  end function\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use wrapper, only: box, pick\n  implicit none\n  type(box) :: b\n  b%v = 42\n  print *, pick(b)\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &dep_f90, &dep_o, None);
    compile_file(&compiler, &wrapper_f90, &wrapper_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &wrapper_o, &dep_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("42"),
        "expected wrapper generic to dispatch to pick_box, got:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_type_bound_result_guides_operator_generic() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=imported_type_bound_result_guides_operator_generic count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let string_f90 = dir.join("string_mod.f90");
    let list_f90 = dir.join("list_mod.f90");
    let main_f90 = dir.join("main.f90");
    let string_o = dir.join("string_mod.o");
    let list_o = dir.join("list_mod.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("test_bin");

    std::fs::write(
        &string_f90,
        r#"module string_mod
  implicit none
  private
  public :: string_type, operator(==)

  type :: string_type
    character(len=:), allocatable :: raw
  end type

  interface string_type
    module procedure new_string
  end interface

  interface operator(==)
    module procedure eq_char_string
    module procedure eq_string_char
    module procedure eq_string_string
  end interface

contains
  function new_string(raw) result(s)
    character(len=*), intent(in) :: raw
    type(string_type) :: s
    s%raw = raw
  end function

  logical function eq_string_string(lhs, rhs)
    type(string_type), intent(in) :: lhs
    type(string_type), intent(in) :: rhs
    eq_string_string = allocated(lhs%raw) .eqv. allocated(rhs%raw)
    if (eq_string_string .and. allocated(lhs%raw)) eq_string_string = lhs%raw == rhs%raw
  end function

  logical function eq_string_char(lhs, rhs)
    type(string_type), intent(in) :: lhs
    character(len=*), intent(in) :: rhs
    eq_string_char = allocated(lhs%raw)
    if (eq_string_char) eq_string_char = lhs%raw == rhs
  end function

  logical function eq_char_string(lhs, rhs)
    character(len=*), intent(in) :: lhs
    type(string_type), intent(in) :: rhs
    eq_char_string = allocated(rhs%raw)
    if (eq_char_string) eq_char_string = lhs == rhs%raw
  end function
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &list_f90,
        r#"module list_mod
  use string_mod, only: string_type
  implicit none
  private
  public :: list_type

  type :: list_type
    type(string_type) :: value
  contains
    procedure :: get
  end type

contains
  function get(list) result(value)
    class(list_type), intent(in) :: list
    type(string_type) :: value
    value = list%value
  end function
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        r#"program p
  use string_mod, only: string_type, operator(==)
  use list_mod, only: list_type
  implicit none
  type(list_type) :: list

  list%value = string_type("ok")
  if (.not. (list%get() == string_type("ok"))) error stop 1
  print *, "ok"
end program
"#,
    )
    .unwrap();

    compile_file(&compiler, &string_f90, &string_o, None);
    compile_file(&compiler, &list_f90, &list_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &list_o, &string_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "expected imported TBP result to dispatch eq_string_string, got:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn module_private_default() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=module_private_default count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    // F2008 §12.2.3.2: submodules of a module see *all* parent entities,
    // including the privates. The .amod must therefore round-trip private
    // module variables — but tagged `private` so module-level USE
    // associations reject them while submodule host association accepts.
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("mod.f90");
    let mod_o = dir.join("mod.o");

    std::fs::write(&mod_f90,
        "module priv_mod\n  implicit none\n  private\n  integer, public :: pub_val = 42\n  integer :: priv_val = 99\nend module\n"
    ).unwrap();
    compile_file(&compiler, &mod_f90, &mod_o, None);

    let amod = std::fs::read_to_string(dir.join("priv_mod.amod")).unwrap();
    let pub_line = amod
        .lines()
        .find(|l| l.contains("pub_val"))
        .expect("pub_val should appear in .amod");
    assert!(
        !pub_line.contains("private"),
        "pub_val should not carry the `private` annotation: {pub_line}"
    );
    let priv_line = amod
        .lines()
        .find(|l| l.contains("priv_val"))
        .expect("priv_val should appear in .amod (with `private` annotation) so submodule host association can resolve it");
    assert!(
        priv_line.contains("private"),
        "priv_val must be tagged `private` in the .amod: {priv_line}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// l07: a separately-compiled submodule whose body implements a parent
// MODULE FUNCTION must return the right type. The result variable's type
// comes from the parent interface via the `.amod`; before l07 it fell to
// implicit typing (an integer result named `r` became REAL, returned in a
// different register than the caller read) — a silent wrong answer. Covers
// both the with-args and no-arg function forms plus a subroutine control.
#[test]
fn cross_tu_submodule_scalar_function_returns_correct_type() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_submodule_scalar_function_returns_correct_type count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let parent_f90 = dir.join("sm_parent.f90");
    let child_f90 = dir.join("sm_child.f90");
    let main_f90 = dir.join("sm_main.f90");
    let parent_o = dir.join("sm_parent.o");
    let child_o = dir.join("sm_child.o");
    let main_o = dir.join("sm_main.o");
    let binary = dir.join("sm_bin");

    std::fs::write(
        &parent_f90,
        r#"module sm
  implicit none
  interface
    module function dbl(x) result(r)
      integer, intent(in) :: x
      integer :: r
    end function
    module function answer() result(r)
      integer :: r
    end function
    module subroutine setit(v)
      integer, intent(out) :: v
    end subroutine
  end interface
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        r#"submodule (sm) sm_impl
contains
  module procedure dbl
    r = 2 * x
  end procedure
  module procedure answer
    r = 42
  end procedure
  module procedure setit
    v = 99
  end procedure
end submodule
"#,
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        r#"program p
  use sm
  implicit none
  integer :: v
  if (dbl(21) /= 42) error stop 1
  if (answer() /= 42) error stop 2
  call setit(v)
  if (v /= 99) error stop 3
  print *, "ok"
end program
"#,
    )
    .unwrap();

    compile_file(&compiler, &parent_f90, &parent_o, None);
    compile_file(&compiler, &child_f90, &child_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &child_o, &parent_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "cross-TU submodule scalar function returned wrong value (or wrong register):\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_submodule_array_function_passes_explicit_shape_actuals_by_data_pointer() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_submodule_array_function_passes_explicit_shape_actuals_by_data_pointer count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let parent_f90 = dir.join("cross_parent.f90");
    let child_f90 = dir.join("cross_child.f90");
    let main_f90 = dir.join("cross_main.f90");
    let parent_o = dir.join("cross_parent.o");
    let child_o = dir.join("cross_child.o");
    let main_o = dir.join("cross_main.o");
    let binary = dir.join("cross_bin");

    std::fs::write(
        &parent_f90,
        r#"module cross_mod
  implicit none
  interface
    module function cross_i(a, b) result(res)
      integer, intent(in) :: a(3), b(3)
      integer :: res(3)
    end function
  end interface
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        r#"submodule (cross_mod) cross_impl
contains
  pure module function cross_i(a, b) result(res)
    integer, intent(in) :: a(3), b(3)
    integer :: res(3)
    res(1) = a(2) * b(3) - a(3) * b(2)
    res(2) = a(3) * b(1) - a(1) * b(3)
    res(3) = a(1) * b(2) - a(2) * b(1)
  end function
end submodule
"#,
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        r#"program p
  use cross_mod, only: cross_i
  implicit none
  integer :: u(3), v(3), expected(3), diff(3)

  u = [1, 0, 0]
  v = [0, 1, 0]
  expected = [0, 0, 1]
  diff = expected - cross_i(u, v)
  if (any(diff /= 0)) error stop 1
  print *, "ok"
end program
"#,
    )
    .unwrap();

    compile_file(&compiler, &parent_f90, &parent_o, None);
    compile_file(&compiler, &child_f90, &child_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &child_o, &parent_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "cross-TU submodule array result returned wrong values:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_submodule_allocatable_array_result_preserves_amod_abi() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_submodule_allocatable_array_result_preserves_amod_abi count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let parent_f90 = dir.join("alloc_parent.f90");
    let child_f90 = dir.join("alloc_child.f90");
    let main_f90 = dir.join("alloc_main.f90");
    let parent_o = dir.join("alloc_parent.o");
    let child_o = dir.join("alloc_child.o");
    let main_o = dir.join("alloc_main.o");
    let binary = dir.join("alloc_bin");

    std::fs::write(
        &parent_f90,
        r#"module alloc_parent
  implicit none
  interface
    module function make_square(n) result(a)
      integer, intent(in) :: n
      real, allocatable :: a(:, :)
    end function
  end interface
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        r#"submodule (alloc_parent) alloc_impl
contains
  module function make_square(n) result(a)
    integer, intent(in) :: n
    real, allocatable :: a(:, :)
    integer :: i
    allocate(a(n, n))
    a = 0.0
    do i = 1, n
      a(i, i) = real(i)
    end do
  end function
end submodule
"#,
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        r#"program p
  use alloc_parent, only: make_square
  implicit none
  real, allocatable :: a(:, :)

  a = make_square(3)
  if (.not. allocated(a)) error stop 1
  if (size(a, 1) /= 3 .or. size(a, 2) /= 3) error stop 2
  if (abs(a(1, 1) - 1.0) > 1.0e-6) error stop 3
  if (abs(a(2, 2) - 2.0) > 1.0e-6) error stop 4
  if (abs(a(3, 3) - 3.0) > 1.0e-6) error stop 5
  print *, "ok"
end program
"#,
    )
    .unwrap();

    compile_file(&compiler, &parent_f90, &parent_o, None);
    let amod = std::fs::read_to_string(dir.join("alloc_parent.amod")).unwrap();
    assert!(
        amod.contains("@function make_square -> real, result_allocatable, result_rank=2"),
        "allocatable module-function result ABI missing from parent .amod:\n{}",
        amod
    );
    compile_file(&compiler, &child_f90, &child_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &child_o, &parent_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "cross-TU submodule allocatable array result returned wrong values:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_submodule_scalar_function_call_broadcasts_to_descriptor_array() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_submodule_scalar_function_call_broadcasts_to_descriptor_array count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let parent_f90 = dir.join("broadcast_parent.f90");
    let child_f90 = dir.join("broadcast_child.f90");
    let main_f90 = dir.join("broadcast_main.f90");
    let parent_o = dir.join("broadcast_parent.o");
    let child_o = dir.join("broadcast_child.o");
    let main_o = dir.join("broadcast_main.o");
    let binary = dir.join("broadcast_bin");

    std::fs::write(
        &parent_f90,
        r#"module broadcast_parent
  implicit none
  interface
    module function wrap(a, order) result(e)
      real, intent(in) :: a(:, :)
      integer, optional, intent(in) :: order
      real, allocatable :: e(:, :)
    end function
    module subroutine mark_inplace(a, order)
      real, intent(inout) :: a(:, :)
      integer, optional, intent(in) :: order
    end subroutine
  end interface
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        r#"submodule (broadcast_parent) broadcast_impl
contains
  module function wrap(a, order) result(e)
    real, intent(in) :: a(:, :)
    integer, optional, intent(in) :: order
    real, allocatable :: e(:, :)
    e = a
    call mark_inplace(e, order)
  end function

  module subroutine mark_inplace(a, order)
    real, intent(inout) :: a(:, :)
    integer, optional, intent(in) :: order
    if (present(order)) then
      a = real(order)
    else
      a = 11.0
    end if
  end subroutine
end submodule
"#,
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        r#"program p
  use broadcast_parent, only: wrap
  implicit none
  real :: a(2, 2)
  real, allocatable :: e(:, :)

  a = 1.0
  e = wrap(a)
  if (any(abs(e - 11.0) > 1.0e-6)) error stop 1
  e = wrap(a, order=3)
  if (any(abs(e - 3.0) > 1.0e-6)) error stop 2
  print *, "ok"
end program
"#,
    )
    .unwrap();

    compile_file(&compiler, &parent_f90, &parent_o, None);
    compile_file(&compiler, &child_f90, &child_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &child_o, &parent_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "cross-TU submodule scalar function-call broadcast failed:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn submodule_runtime_shape_local_uses_dummy_size_not_global_shadow() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=submodule_runtime_shape_local_uses_dummy_size_not_global_shadow count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let parent_f90 = dir.join("shape_parent.f90");
    let child_f90 = dir.join("shape_child.f90");
    let main_f90 = dir.join("shape_main.f90");
    let parent_o = dir.join("shape_parent.o");
    let child_o = dir.join("shape_child.o");
    let main_o = dir.join("shape_main.o");
    let binary = dir.join("shape_bin");

    std::fs::write(
        &parent_f90,
        r#"module shape_parent
  implicit none
  real :: a(1, 1)
  interface
    module subroutine fill(a)
      real, intent(inout) :: a(:, :)
    end subroutine
  end interface
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        r#"submodule (shape_parent) shape_impl
contains
  module subroutine fill(a)
    real, intent(inout) :: a(:, :)
    real :: tmp(size(a, 1), size(a, 2))
    integer :: i, j

    do j = 1, size(a, 2)
      do i = 1, size(a, 1)
        tmp(i, j) = 10.0 * real(i) + real(j)
      end do
    end do
    a = tmp
  end subroutine
end submodule
"#,
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        r#"program p
  use shape_parent, only: fill
  implicit none
  real :: x(5, 5)

  x = 0.0
  call fill(x)
  if (abs(x(5, 5) - 55.0) > 1.0e-6) error stop 1
  print *, "ok"
end program
"#,
    )
    .unwrap();

    compile_file(&compiler, &parent_f90, &parent_o, None);
    compile_file(&compiler, &child_f90, &child_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &child_o, &parent_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "submodule runtime-shape local used the wrong size() binding:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// l07 DoD: the multi-source driver (`armfortas a.f90 b.f90 ...` in one
// invocation) topologically orders submodules after their parents, even
// when files are given in the worst order. Before l07's dep_scan support,
// the submodule compiled before its parent's `.amod` existed and produced
// a silent wrong answer.
#[test]
fn multi_source_submodule_wrong_order_builds_and_runs() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=multi_source_submodule_wrong_order_builds_and_runs count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let parent_f90 = dir.join("ms_parent.f90");
    let child_f90 = dir.join("ms_child.f90");
    let main_f90 = dir.join("ms_main.f90");
    let binary = dir.join("ms_bin");

    std::fs::write(
        &parent_f90,
        "module ms\n  implicit none\n  interface\n    module function dbl(x) result(r)\n      integer, intent(in) :: x\n      integer :: r\n    end function\n  end interface\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        "submodule (ms) ms_impl\ncontains\n  module procedure dbl\n    r = 2 * x\n  end procedure\nend submodule\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use ms\n  if (dbl(21) /= 42) error stop 1\n  print *, \"ok\"\nend program\n",
    )
    .unwrap();

    // Deliberately worst order: child before parent.
    let result = std::process::Command::new(&compiler)
        .current_dir(&dir)
        .args([
            child_f90.to_str().unwrap(),
            parent_f90.to_str().unwrap(),
            main_f90.to_str().unwrap(),
            "-o",
            binary.to_str().unwrap(),
        ])
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "multi-source submodule build failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "multi-source submodule wrong-order run gave wrong answer:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// l07: a type-bound procedure whose target is a separate module procedure,
// with the module and its submodule in separate TUs. Exercises the
// TBP-thunk ownership rule across compilation units (the thunk must have
// exactly one owning object, or the link fails with a duplicate symbol).
#[test]
fn cross_tu_tbp_targets_submodule_procedure() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_tbp_targets_submodule_procedure count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("tb_mod.f90");
    let child_f90 = dir.join("tb_child.f90");
    let main_f90 = dir.join("tb_main.f90");
    let binary = dir.join("tb_bin");

    std::fs::write(
        &mod_f90,
        "module tb\n  implicit none\n  type :: counter\n    integer :: n = 0\n  contains\n    procedure :: bump\n  end type\n  interface\n    module subroutine bump(self, by)\n      class(counter), intent(inout) :: self\n      integer, intent(in) :: by\n    end subroutine\n  end interface\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        "submodule (tb) tb_impl\ncontains\n  module procedure bump\n    self%n = self%n + by\n  end procedure\nend submodule\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use tb\n  type(counter) :: c\n  call c%bump(5)\n  call c%bump(7)\n  if (c%n /= 12) error stop 1\n  print *, \"ok\"\nend program\n",
    )
    .unwrap();

    // Worst order again.
    let result = std::process::Command::new(&compiler)
        .current_dir(&dir)
        .args([
            child_f90.to_str().unwrap(),
            mod_f90.to_str().unwrap(),
            main_f90.to_str().unwrap(),
            "-o",
            binary.to_str().unwrap(),
        ])
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "cross-TU TBP→SMP build failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "cross-TU TBP→SMP wrong result:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// A parent module can emit the owning vtable before submodule procedure bodies
// are compiled. Concrete vtable slots must still point at those external
// module-procedure symbols; otherwise a wrapper that dispatches through the
// deferred binding lands on a null slot at runtime.
#[test]
fn parent_vtable_references_submodule_tbp_target() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=parent_vtable_references_submodule_tbp_target count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("vt_parent.f90");
    let child_f90 = dir.join("vt_child.f90");
    let main_f90 = dir.join("vt_main.f90");
    let mod_o = dir.join("vt_parent.o");
    let child_o = dir.join("vt_child.o");
    let main_o = dir.join("vt_main.o");
    let binary = dir.join("vt_bin");

    std::fs::write(
        &mod_f90,
        "module vt_parent\n  implicit none\n  type :: counter\n    integer :: n = 0\n  contains\n    procedure :: bump\n    procedure :: ensure\n  end type\n  interface\n    module subroutine bump(self, by)\n      class(counter), intent(inout) :: self\n      integer, intent(in) :: by\n    end subroutine\n  end interface\ncontains\n  subroutine ensure(self)\n    class(counter), intent(inout) :: self\n    call self%bump(5)\n  end subroutine\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        "submodule (vt_parent) vt_child\ncontains\n  module procedure bump\n    self%n = self%n + by\n  end procedure\nend submodule\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use vt_parent\n  implicit none\n  type(counter) :: c\n  call c%ensure()\n  if (c%n /= 5) error stop 1\n  print *, \"ok\"\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &mod_f90, &mod_o, None);
    compile_file(&compiler, &child_f90, &child_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&mod_o, &child_o, &main_o], &binary);

    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "submodule-backed TBP vtable dispatch failed:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn local_child_vtable_keeps_imported_tbp_target_over_same_abi_interface_name() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=local_child_vtable_keeps_imported_tbp_target_over_same_abi_interface_name count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let rk_a_f90 = dir.join("rk_a.f90");
    let rk_b_f90 = dir.join("rk_b.f90");
    let facade_f90 = dir.join("facade.f90");
    let main_f90 = dir.join("main.f90");
    let rk_a_o = dir.join("rk_a.o");
    let rk_b_o = dir.join("rk_b.o");
    let facade_o = dir.join("facade.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("facade_vtable_bin");

    std::fs::write(
        &rk_a_f90,
        "module rk_a\n  implicit none\n  type, abstract :: rk_class\n  contains\n    procedure(step_func), deferred :: step\n    procedure :: integrate => a_integrate\n  end type\n  abstract interface\n    subroutine step_func(self)\n      import :: rk_class\n      class(rk_class), intent(inout) :: self\n    end subroutine\n  end interface\n  type, extends(rk_class) :: rk8_10_class\n  contains\n    procedure :: step => rk8_10\n  end type\ncontains\n  subroutine a_integrate(self)\n    class(rk_class), intent(inout) :: self\n    call self%step()\n  end subroutine\n  subroutine rk8_10(self)\n    class(rk8_10_class), intent(inout) :: self\n  end subroutine\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &rk_b_f90,
        "module rk_b\n  implicit none\n  type, abstract :: other_class\n  contains\n    procedure(step_func), deferred :: step\n    procedure :: integrate => b_integrate\n  end type\n  abstract interface\n    subroutine step_func(self)\n      import :: other_class\n      class(other_class), intent(inout) :: self\n    end subroutine\n  end interface\ncontains\n  subroutine b_integrate(self)\n    class(other_class), intent(inout) :: self\n  end subroutine\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &facade_f90,
        "module facade\n  use rk_a\n  use rk_b\n  implicit none\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use facade\n  implicit none\n  type, extends(rk8_10_class) :: spacecraft\n    integer :: marker = 0\n  end type\n  type(spacecraft) :: s\n  call s%integrate()\n  print *, \"ok\"\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &rk_a_f90, &rk_a_o, None);
    compile_file(&compiler, &rk_b_f90, &rk_b_o, Some(&dir));
    compile_file(&compiler, &facade_f90, &facade_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));

    let undef = undefined_symbols(&main_o);
    assert!(
        !undef.iter().any(|sym| {
            sym.trim_start_matches('_')
                == "afs_modproc_rk_b_step_func"
        }),
        "local child vtable should keep rk_a's imported target, not rk_b's interface placeholder: {:?}",
        undef
    );

    link_files(&[&rk_a_o, &rk_b_o, &facade_o, &main_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "facade-imported child vtable dispatch failed:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// l08: vtable slot ordering must be identical whether a TU computes it
// from the type's source or from its `.amod`. The owner module dispatches
// `a()`/`b()` through `class(base)` (source-visible layout); the consumer
// dispatches the same calls on the same dynamic type seen only through
// the `.amod` (amod-only layout). The child overrides `a` (keeps the
// parent slot) and adds `c` (new slot), so a slot-order skew between the
// two views would call the wrong method and the two sums would diverge.
#[test]
fn cross_tu_vtable_slots_match_source_and_amod_views() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_vtable_slots_match_source_and_amod_views count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("vt_mod.f90");
    let main_f90 = dir.join("vt_main.f90");
    let binary = dir.join("vt_bin");

    std::fs::write(
        &mod_f90,
        "module vt\n\
         implicit none\n\
         type :: base\n\
         contains\n\
         procedure :: a => a_base\n\
         procedure :: b => b_base\n\
         end type\n\
         type, extends(base) :: child\n\
         contains\n\
         procedure :: a => a_child\n\
         procedure :: c => c_child\n\
         end type\n\
         contains\n\
         integer function a_base(self)\n\
         class(base), intent(in) :: self\n\
         a_base = 1\n\
         end function\n\
         integer function b_base(self)\n\
         class(base), intent(in) :: self\n\
         b_base = 2\n\
         end function\n\
         integer function a_child(self)\n\
         class(child), intent(in) :: self\n\
         a_child = 10\n\
         end function\n\
         integer function c_child(self)\n\
         class(child), intent(in) :: self\n\
         c_child = 30\n\
         end function\n\
         ! Source-visible dispatch: compiled in the owner TU.\n\
         integer function via_owner(x)\n\
         class(base), intent(in) :: x\n\
         via_owner = x%a() + x%b() * 100\n\
         end function\n\
         end module\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n\
         use vt\n\
         implicit none\n\
         class(base), allocatable :: s\n\
         integer :: owner_sum, consumer_sum\n\
         allocate(child :: s)\n\
         owner_sum = via_owner(s)            ! source-visible layout\n\
         consumer_sum = s%a() + s%b() * 100  ! amod-only layout\n\
         if (owner_sum /= 210) error stop 1\n\
         if (consumer_sum /= 210) error stop 2\n\
         print *, \"ok\"\n\
         end program\n",
    )
    .unwrap();

    let result = std::process::Command::new(&compiler)
        .current_dir(&dir)
        .args([
            mod_f90.to_str().unwrap(),
            main_f90.to_str().unwrap(),
            "-o",
            binary.to_str().unwrap(),
        ])
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "cross-TU vtable slot build failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "cross-TU vtable slot ordering mismatch (source vs amod):\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn enumeration_type_amod_roundtrip() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=enumeration_type_amod_roundtrip count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    // Orphaned l03 deferral (was l07's row): USE of a module that
    // defines an F2023 ENUMERATION TYPE must re-register the type and
    // its typed enumerator constants from the .amod — declaration,
    // assignment, NEXT, and HUGE all through the round-trip.
    multifile_test_flags(
        "module emod\n  implicit none\n  enumeration type :: color\n    enumerator :: red, green, blue\n  end enumeration type\nend module\n",
        "program p\n  use emod\n  implicit none\n  type(color) :: c\n  c = green\n  c = next(c)\n  print '(i0,1x,i0)', int(c), int(huge(c))\nend program\n",
        "3 3",
        &["--std=f2023"],
    );
}

#[test]
fn assumed_size_integer_dummy_cross_module_passes_data_address() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=assumed_size_integer_dummy_cross_module_passes_data_address count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    // A module procedure with an assumed-size `buf(*)` dummy passes its
    // argument by bare data address, not by descriptor. The `.amod`
    // records this correctly (no `descriptor` attr), but the consumer
    // reconstructs the dummy as AssumedShape (rank-based fallback), and
    // `descriptor_param_mask_for_lookup` used to let that lossy scope
    // reconstruction override the authoritative `.amod` mask — so the
    // caller passed a descriptor pointer the callee then read as data
    // (garbage element reads). Same-file callers were unaffected, which
    // is why every single-file fixture missed it.
    multifile_test(
        "module asize_i\n  implicit none\ncontains\n  integer function count_pos(buf) result(n)\n    integer, intent(in) :: buf(*)\n    n = 0\n    do while (buf(n + 1) /= 0)\n      n = n + 1\n    end do\n  end function\nend module\n",
        "program p\n  use asize_i\n  implicit none\n  integer :: a(6)\n  a = 0\n  a(1) = 7\n  a(2) = 8\n  a(3) = 9\n  print '(i0)', count_pos(a)\nend program\n",
        "3",
    );
}

#[test]
fn assumed_size_cchar_dummy_cross_module_reads_correctly() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=assumed_size_cchar_dummy_cross_module_reads_correctly count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    // The C-interop shape that crashed 7 fgof libraries: a NUL-terminator
    // scan over a `character(kind=c_char) :: buf(*)` assumed-size dummy in
    // a separately compiled module. Before the descriptor-mask fix the
    // caller passed a descriptor, the callee read garbage element values,
    // the scan overran, and the resulting bad length fed memmove and
    // SIGSEGV'd.
    multifile_test(
        "module asize_c\n  use iso_c_binding, only : c_char, c_null_char\n  implicit none\ncontains\n  integer function clen(buf) result(n)\n    character(kind=c_char), intent(in) :: buf(*)\n    n = 0\n    do while (buf(n + 1) /= c_null_char)\n      n = n + 1\n    end do\n  end function\nend module\n",
        "program p\n  use asize_c\n  use iso_c_binding, only : c_char, c_null_char\n  implicit none\n  character(kind=c_char) :: b(8)\n  integer :: i\n  do i = 1, 8\n    b(i) = c_null_char\n  end do\n  b(1) = 'x'\n  b(2) = 'y'\n  b(3) = 'z'\n  print '(i0)', clen(b)\nend program\n",
        "3",
    );
}

#[test]
fn generic_dispatch_allocatable_rank2_component_cross_module() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=generic_dispatch_allocatable_rank2_component_cross_module count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    // The stdlib sparse `sort_coo(COO%index, ...)` regression: dispatching a
    // generic on an allocatable rank-2 derived-type component whose type is
    // defined in a separately compiled module. A deferred-shape component's
    // `.amod` layout carried empty dims, so its declared rank (2) was lost;
    // the actual reported rank 1 and no rank-2 specific matched. Fixed by
    // seeding a deferred-shape component's dims with `vec![(1, 0); rank]` so
    // `dims.len()` preserves the declared rank. Correct dispatch binds the
    // 4-arg specific: 5 + 7 + 0 = 12.
    multifile_test(
        "module gdx_types\n  implicit none\n  integer, parameter :: ilp = 4\n  type :: base_t\n    integer(ilp) :: nrows = 0, ncols = 0, nnz = 0\n  end type\n  type, extends(base_t) :: coo_t\n    integer(ilp), allocatable :: index(:,:)\n  end type\nend module\nmodule gdx_ops\n  use gdx_types\n  implicit none\n  interface sort_coo\n    module procedure sort4\n    module procedure sort5\n  end interface\ncontains\n  subroutine sort4(a, n, num_rows, num_cols)\n    integer(ilp), intent(inout) :: a(2,*)\n    integer(ilp), intent(inout) :: n\n    integer(ilp), intent(in) :: num_rows, num_cols\n    n = num_rows + num_cols + a(1,1)\n  end subroutine\n  subroutine sort5(a, data, n, num_rows, num_cols)\n    integer(ilp), intent(inout) :: a(2,*)\n    real, intent(inout) :: data(*)\n    integer(ilp), intent(inout) :: n\n    integer(ilp), intent(in) :: num_rows, num_cols\n    n = num_rows\n  end subroutine\nend module\n",
        "program p\n  use gdx_types\n  use gdx_ops\n  implicit none\n  type(coo_t) :: c\n  allocate(c%index(2,10))\n  c%index = 0\n  c%nnz = 3\n  c%nrows = 5\n  c%ncols = 7\n  call sort_coo(c%index, c%nnz, c%nrows, c%ncols)\n  print '(i0)', c%nnz\nend program\n",
        "12",
    );
}

#[test]
fn generic_dispatch_block_local_scalar_not_shadowed_by_foreign_dummy_rank() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=generic_dispatch_block_local_scalar_not_shadowed_by_foreign_dummy_rank count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    // The order-dependent stdlib `dense(A)` / `check(...)` regression. A
    // scalar derived-type actual `a` declared inside a BLOCK isn't in the
    // procedure scope, so the generic dispatcher's rank cross-check fell
    // through to a whole-symbol-table lookup and picked up a same-named
    // rank-1 dummy from the use-associated noise_mod — inferring the scalar
    // actual as rank 1 and matching no rank-0 specific. Which foreign `a`
    // won depended on module load order. The rank cross-check for a known
    // local now stays in the current scope. Correct dispatch: 5 + 100.
    multifile_test(
        "module noise_mod\n  implicit none\ncontains\n  subroutine noise(a)\n    integer, intent(inout) :: a(:)\n    a = a + 1\n  end subroutine\nend module\nmodule wt_mod\n  implicit none\n  type :: wt\n    integer :: v = 0\n  end type\n  interface widen\n    module procedure widen_t\n  end interface\ncontains\n  integer function widen_t(a) result(r)\n    type(wt), intent(in) :: a\n    r = a%v + 100\n  end function\nend module\n",
        "program p\n  use noise_mod\n  use wt_mod\n  implicit none\n  block\n    type(wt) :: a\n    a%v = 5\n    print '(i0)', widen(a)\n  end block\nend program\n",
        "105",
    );
}
