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
    if let Err(reason) = armfortas::testing::native_e2e_support() {
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
    if let Err(reason) = armfortas::testing::native_e2e_support() {
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

#[test]
fn module_with_derived_type() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
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
    if let Err(reason) = armfortas::testing::native_e2e_support() {
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
    if let Err(reason) = armfortas::testing::native_e2e_support() {
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
fn use_rename() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
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
    if let Err(reason) = armfortas::testing::native_e2e_support() {
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
    if let Err(reason) = armfortas::testing::native_e2e_support() {
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
    if let Err(reason) = armfortas::testing::native_e2e_support() {
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
    if let Err(reason) = armfortas::testing::native_e2e_support() {
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
    if let Err(reason) = armfortas::testing::native_e2e_support() {
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
    if let Err(reason) = armfortas::testing::native_e2e_support() {
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
