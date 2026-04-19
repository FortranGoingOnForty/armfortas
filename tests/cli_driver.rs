//! Sprint 32 CLI driver tests.
//!
//! Each test exercises one user-visible behaviour of the `armfortas`
//! / `afs` driver via subprocess invocation.  Subprocess use is
//! deliberate — we want to catch wrong-exit-code, wrong-stdout-vs-
//! stderr-routing, and missing-symbol-from-bin issues that an
//! in-process API call wouldn't see.

use std::fs;
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
    std::env::temp_dir().join(format!("afs_cli_{}_{}_{}.{}", stem, pid, id, ext))
}

fn unique_dir(stem: &str) -> PathBuf {
    let dir = unique_path(stem, "dir");
    std::fs::create_dir_all(&dir).expect("cannot create CLI test directory");
    dir
}

fn write_program(text: &str, suffix: &str) -> PathBuf {
    let path = unique_path("src", suffix);
    std::fs::write(&path, text).expect("cannot write CLI test source");
    path
}

fn write_program_in(dir: &std::path::Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("cannot write CLI test source");
    path
}

fn undefined_symbols(path: &std::path::Path) -> Vec<String> {
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

fn compile_c_object(source: &std::path::Path, output: &std::path::Path) {
    let result = Command::new("clang")
        .args([
            "-arch",
            "arm64",
            "-c",
            source.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .expect("failed to spawn clang");
    assert!(
        result.status.success(),
        "clang failed for {}: {}",
        source.display(),
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn version_flag_prints_version_string_to_stdout() {
    let out = Command::new(compiler("armfortas"))
        .arg("--version")
        .output()
        .expect("failed to spawn armfortas");
    assert!(out.status.success(), "exit code: {:?}", out.status);
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("armfortas") && stdout.contains("0.1.0"),
        "unexpected --version output: {}",
        stdout
    );
    // The version string belongs on stdout (not stderr) per
    // gfortran/clang convention; users shell-pipe it.
    assert!(
        out.stderr.is_empty(),
        "stderr should be empty: {:?}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn help_flag_shows_usage_and_exits_zero() {
    let out = Command::new(compiler("armfortas"))
        .arg("--help")
        .output()
        .expect("failed to spawn armfortas");
    assert!(out.status.success(), "--help should succeed");
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("USAGE"), "help missing USAGE line");
    assert!(stdout.contains("--std="), "help missing --std= entry");
}

#[test]
fn dumpversion_prints_just_the_version_number() {
    let out = Command::new(compiler("armfortas"))
        .arg("-dumpversion")
        .output()
        .expect("failed to spawn armfortas");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert_eq!(stdout.trim(), "0.1.0");
}

#[test]
fn afs_alias_runs_the_same_compiler() {
    let out = Command::new(compiler("afs"))
        .arg("--version")
        .output()
        .expect("failed to spawn afs alias");
    assert!(out.status.success());
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.starts_with("afs "),
        "afs --version should identify itself as afs: {}",
        stdout
    );
}

#[test]
fn no_args_prints_help_to_stdout_and_exits_zero() {
    let out = Command::new(compiler("armfortas"))
        .output()
        .expect("failed to spawn armfortas");
    assert!(
        out.status.success(),
        "no-arg invocation should show usage help"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(
        stdout.contains("USAGE"),
        "no-arg invocation should print help to stdout: {}",
        stdout
    );
    assert!(
        out.stderr.is_empty(),
        "no-arg invocation should not print usage to stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
fn no_input_after_flags_prints_help_and_mentions_missing_input() {
    let out = Command::new(compiler("armfortas"))
        .arg("-Wall")
        .output()
        .expect("failed to spawn armfortas");
    assert!(
        out.status.success(),
        "flag-only no-input invocation should exit zero"
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(stdout.contains("USAGE"), "missing help text: {}", stdout);
    assert!(
        stderr.contains("no input file"),
        "expected missing-input note on stderr: {}",
        stderr
    );
}

#[test]
fn dash_c_produces_object_file_only() {
    let src = write_program("module foo\n  integer :: x = 1\nend module\n", "f90");
    let out = unique_path("obj", "o");
    let result = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("compile failed to spawn");
    assert!(
        result.status.success(),
        "-c compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(out.exists(), "-c should produce an object file");
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fixed_form_program_compiles_and_runs() {
    let src = write_program(
        "      PROGRAM P\n      INTEGER I, S\n      S = 0\n      DO 10 I = 1, 3\n         S = S + I\n   10 CONTINUE\n      PRINT *, S\n      END\n",
        "f",
    );
    let out = unique_path("fixed_form", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("fixed-form compile failed to spawn");
    assert!(
        compile.status.success(),
        "fixed-form compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("fixed-form run failed");
    assert!(
        run.status.success(),
        "fixed-form run failed: {:?}",
        run.status
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.trim().ends_with('6'),
        "unexpected fixed-form output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn formatted_char_read_with_size_from_redirected_stdin_compiles_and_runs() {
    let src = write_program(
        "program p\n  use iso_fortran_env, only: input_unit\n  implicit none\n  character(len=16) :: buf\n  integer :: ios, n\n  read(input_unit, '(a)', iostat=ios, advance='no', size=n) buf\n  write(*,'(a,i0)') 'IOS=', ios\n  write(*,'(a,i0)') 'N=', n\n  write(*,'(a,a,a)') 'BUF=<', trim(buf), '>'\nend program\n",
        "f90",
    );
    let out = unique_path("formatted_char_read", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("formatted char read compile failed to spawn");
    assert!(
        compile.status.success(),
        "formatted char read compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let input = unique_path("formatted_char_read_input", "txt");
    std::fs::write(&input, "line\n").expect("cannot write formatted char read input");
    let run = Command::new(&out)
        .stdin(std::fs::File::open(&input).expect("cannot open formatted char read input"))
        .output()
        .expect("formatted char read run failed");
    assert!(
        run.status.success(),
        "formatted char read run failed: {:?}\nstderr: {}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("IOS=0") || stdout.contains("IOS=-2"),
        "expected successful or EOR iostat, got: {}",
        stdout
    );
    assert!(stdout.contains("N=4"), "expected SIZE=4, got: {}", stdout);
    assert!(
        stdout.contains("BUF=<line>"),
        "expected buffer contents, got: {}",
        stdout
    );

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_param_fixed_char_len_preserves_get_command_argument_buffer() {
    let dir = unique_dir("imported_param_char_len");
    let mod_src = write_program_in(
        &dir,
        "cfg.f90",
        "module cfg\n  implicit none\n  integer, parameter :: max_path_len = 32\nend module cfg\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use cfg, only: max_path_len\n  implicit none\n  character(len=max_path_len) :: arg1\n  call get_command_argument(1, arg1)\n  if (trim(arg1) /= '--version') error stop 1\n  print *, trim(arg1)\nend program\n",
    );

    let mod_obj = dir.join("cfg.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("cfg module compile failed to spawn");
    assert!(
        compile_mod.status.success(),
        "cfg module should compile: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("main compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "main should compile with imported fixed char len: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("imported_param_char_len.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            mod_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "imported-param char-len objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .arg("--version")
        .output()
        .expect("run spawn failed");
    assert!(
        run.status.success(),
        "imported-param char-len binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("--version"),
        "imported fixed char len should preserve command argument text: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn module_parameter_alias_from_used_module_initializes_global() {
    let dir = unique_dir("module_param_alias_init");
    let cfg_src = write_program_in(
        &dir,
        "cfg.f90",
        "module cfg\n  implicit none\n  integer, parameter :: base = 7\nend module cfg\n",
    );
    let m_src = write_program_in(
        &dir,
        "m.f90",
        "module m\n  use cfg, only: base\n  implicit none\n  integer, parameter :: alias = base\ncontains\n  function get_alias() result(v)\n    integer :: v\n    v = alias\n  end function get_alias\nend module m\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use m, only: get_alias\n  implicit none\n  if (get_alias() /= 7) error stop 1\n  print *, get_alias()\nend program p\n",
    );

    let cfg_obj = dir.join("cfg.o");
    let compile_cfg = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            cfg_src.to_str().unwrap(),
            "-o",
            cfg_obj.to_str().unwrap(),
        ])
        .output()
        .expect("cfg compile failed to spawn");
    assert!(
        compile_cfg.status.success(),
        "cfg module should compile: {}",
        String::from_utf8_lossy(&compile_cfg.stderr)
    );

    let m_obj = dir.join("m.o");
    let compile_m = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            m_src.to_str().unwrap(),
            "-o",
            m_obj.to_str().unwrap(),
        ])
        .output()
        .expect("m compile failed to spawn");
    assert!(
        compile_m.status.success(),
        "module alias should compile: {}",
        String::from_utf8_lossy(&compile_m.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("main compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "main should compile: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("module_param_alias_init.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            cfg_obj.to_str().unwrap(),
            m_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "module alias objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run spawn failed");
    assert!(
        run.status.success(),
        "module alias binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.trim().ends_with('7'),
        "module alias parameter should preserve imported constant value: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn module_boz_int_parameter_initializes_global_and_exports_value() {
    let dir = unique_dir("module_boz_int_param");
    let m_src = write_program_in(
        &dir,
        "m.f90",
        "module m\n  use iso_c_binding\n  implicit none\n  integer(c_int), parameter :: s_ifdir = int(o'040000', c_int)\n  integer(c_int), parameter :: s_ifmt = int(o'170000', c_int)\ncontains\n  function sum_constants() result(v)\n    integer(c_int) :: v\n    v = s_ifdir + s_ifmt\n  end function sum_constants\nend module m\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_int\n  use m, only: s_ifdir, s_ifmt, sum_constants\n  implicit none\n  if (s_ifdir /= 16384_c_int) error stop 1\n  if (s_ifmt /= 61440_c_int) error stop 2\n  if (sum_constants() /= 77824_c_int) error stop 3\n  print *, s_ifdir, s_ifmt, sum_constants()\nend program p\n",
    );

    let m_obj = dir.join("m.o");
    let compile_m = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            m_src.to_str().unwrap(),
            "-o",
            m_obj.to_str().unwrap(),
        ])
        .output()
        .expect("module compile failed to spawn");
    assert!(
        compile_m.status.success(),
        "module BOZ parameter compile should succeed: {}",
        String::from_utf8_lossy(&compile_m.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("main compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "main should compile against BOZ parameter module: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("module_boz_int_param.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            m_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "BOZ parameter objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run spawn failed");
    assert!(
        run.status.success(),
        "BOZ parameter binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("16384") && stdout.contains("61440") && stdout.contains("77824"),
        "module BOZ parameter values should survive globals and use-association: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_param_char_dummy_element_assignment_runs() {
    let dir = unique_dir("imported_param_char_dummy");
    let cfg_src = write_program_in(
        &dir,
        "cfg.f90",
        "module cfg\n  implicit none\n  integer, parameter :: max_token_len = 32\nend module cfg\n",
    );
    let ops_src = write_program_in(
        &dir,
        "ops.f90",
        "module ops\n  use cfg, only: max_token_len\n  implicit none\ncontains\n  subroutine set_first(words)\n    character(len=max_token_len), intent(inout) :: words(:)\n    character(len=max_token_len) :: tmp\n    tmp = 'hello'\n    words(1) = tmp\n  end subroutine set_first\nend module ops\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use cfg, only: max_token_len\n  use ops, only: set_first\n  implicit none\n  character(len=max_token_len), allocatable :: words(:)\n  allocate(words(2))\n  words = ''\n  call set_first(words)\n  print *, trim(words(1))\nend program p\n",
    );

    let cfg_obj = dir.join("cfg.o");
    let compile_cfg = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            cfg_src.to_str().unwrap(),
            "-o",
            cfg_obj.to_str().unwrap(),
        ])
        .output()
        .expect("cfg compile failed to spawn");
    assert!(
        compile_cfg.status.success(),
        "cfg module should compile: {}",
        String::from_utf8_lossy(&compile_cfg.stderr)
    );

    let ops_obj = dir.join("ops.o");
    let compile_ops = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            ops_src.to_str().unwrap(),
            "-o",
            ops_obj.to_str().unwrap(),
        ])
        .output()
        .expect("ops compile failed to spawn");
    assert!(
        compile_ops.status.success(),
        "imported-param char dummy assignment should compile: {}",
        String::from_utf8_lossy(&compile_ops.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("main compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "main should compile: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("imported_param_char_dummy.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            cfg_obj.to_str().unwrap(),
            ops_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "imported-param char dummy objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run spawn failed");
    assert!(
        run.status.success(),
        "imported-param char dummy binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("hello"),
        "dummy char assignment should preserve fixed imported length: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_param_char_section_assignment_preserves_elements() {
    let dir = unique_dir("imported_param_char_section");
    let cfg_src = write_program_in(
        &dir,
        "cfg.f90",
        "module cfg\n  implicit none\n  integer, parameter :: max_token_len = 32\nend module cfg\n",
    );
    let ops_src = write_program_in(
        &dir,
        "ops.f90",
        "module ops\n  use cfg, only: max_token_len\n  implicit none\ncontains\n  subroutine grow(words, current_size)\n    character(len=max_token_len), allocatable, intent(inout) :: words(:)\n    integer, intent(inout) :: current_size\n    character(len=max_token_len), allocatable :: new_words(:)\n    integer :: new_size\n    new_size = current_size * 2\n    allocate(new_words(new_size))\n    new_words(1:current_size) = words(1:current_size)\n    call move_alloc(new_words, words)\n    current_size = new_size\n  end subroutine grow\nend module ops\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use cfg, only: max_token_len\n  use ops, only: grow\n  implicit none\n  character(len=max_token_len), allocatable :: words(:)\n  integer :: n\n  n = 2\n  allocate(words(n))\n  words = ''\n  words(1) = 'one'\n  words(2) = 'two'\n  call grow(words, n)\n  print *, trim(words(1)), trim(words(2)), n\nend program p\n",
    );

    let cfg_obj = dir.join("cfg.o");
    let compile_cfg = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            cfg_src.to_str().unwrap(),
            "-o",
            cfg_obj.to_str().unwrap(),
        ])
        .output()
        .expect("cfg compile failed to spawn");
    assert!(
        compile_cfg.status.success(),
        "cfg module should compile: {}",
        String::from_utf8_lossy(&compile_cfg.stderr)
    );

    let ops_obj = dir.join("ops.o");
    let compile_ops = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            ops_src.to_str().unwrap(),
            "-o",
            ops_obj.to_str().unwrap(),
        ])
        .output()
        .expect("ops compile failed to spawn");
    assert!(
        compile_ops.status.success(),
        "imported-param char section assignment should compile: {}",
        String::from_utf8_lossy(&compile_ops.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("main compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "main should compile: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("imported_param_char_section.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            cfg_obj.to_str().unwrap(),
            ops_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "imported-param char section objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run spawn failed");
    assert!(
        run.status.success(),
        "imported-param char section binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("one") && stdout.contains("two") && stdout.contains('4'),
        "char section assignment should preserve copied elements: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn select_lowering_coerces_mixed_width_branch_values() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: x\n  integer(8) :: y\n  y = 7_8\n  if (y > 0_8) then\n    x = 1\n  else\n    x = y\n  end if\n  print *, x\nend program\n",
        "f90",
    );
    let out = unique_path("select_mixed_width", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("mixed-width select compile failed to spawn");
    assert!(
        compile.status.success(),
        "mixed-width select compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(
        out.exists(),
        "mixed-width select should produce an object file"
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn max_intrinsic_coerces_mixed_width_integer_args() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: x\n  integer(8) :: y\n  y = 7_8\n  x = max(1, y)\n  print *, x\nend program\n",
        "f90",
    );
    let out = unique_path("max_mixed_width", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("mixed-width max compile failed to spawn");
    assert!(
        compile.status.success(),
        "mixed-width max compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(
        out.exists(),
        "mixed-width max should produce an object file"
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn counted_do_coerces_mixed_width_bounds() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=5) :: s\n  integer :: i, total\n  s = 'abc  '\n  total = 0\n  do i = len_trim(s), 1, -1\n    total = total + i\n  end do\n  print *, total\nend program\n",
        "f90",
    );
    let out = unique_path("do_mixed_width", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("mixed-width DO compile failed to spawn");
    assert!(
        compile.status.success(),
        "mixed-width DO compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(out.exists(), "mixed-width DO should produce an object file");

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn runtime_sized_local_character_uses_runtime_string_support() {
    let src = write_program(
        "subroutine f(input, trimmed)\n  implicit none\n  character(len=*), intent(in) :: input\n  integer, intent(out) :: trimmed\n  character(len=len(input)) :: working_input\n  working_input = input\n  trimmed = len_trim(working_input)\nend subroutine\n",
        "f90",
    );
    let out = unique_path("runtime_char_local", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("runtime-sized local character compile failed to spawn");
    assert!(
        compile.status.success(),
        "runtime-sized local character compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&out);
    assert!(
        undefined.iter().any(|sym| sym == "_afs_len_trim"),
        "runtime-sized local character should call afs_len_trim, undefineds were: {:?}",
        undefined
    );
    assert!(
        !undefined.iter().any(|sym| sym == "_working_input"),
        "runtime-sized local character should not lower to an external working_input call: {:?}",
        undefined
    );
    assert!(
        !undefined.iter().any(|sym| sym == "_len_trim"),
        "runtime-sized local character should not lower to a raw len_trim symbol: {:?}",
        undefined
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn assumed_length_character_dummy_keeps_hidden_length_abi() {
    let src = write_program(
        "subroutine f(prompt_str, first)\n  implicit none\n  character(len=*), intent(in) :: prompt_str\n  character(len=1), intent(out) :: first\n  first = prompt_str(1:1)\nend subroutine\n",
        "f90",
    );
    let out = unique_path("assumed_len_dummy", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("assumed-length dummy compile failed to spawn");
    assert!(
        compile.status.success(),
        "assumed-length dummy compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&out);
    assert!(
        !undefined.iter().any(|sym| sym == "_prompt_str"),
        "assumed-length dummy should not become an external prompt_str call: {:?}",
        undefined
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fixed_char_out_dummy_writes_back_to_caller() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: pos\n  character(len=1) :: op\n  pos = find_op('6*7', op)\n  if (pos /= 2) error stop 1\n  if (op /= '*') error stop 2\n  print *, pos, op\ncontains\n  function find_op(expr, op) result(pos)\n    character(len=*), intent(in) :: expr\n    character(len=1), intent(out) :: op\n    integer :: pos, i\n    pos = 0\n    op = ' '\n    do i = len_trim(expr), 1, -1\n      if (expr(i:i) == '*') then\n        pos = i\n        op = expr(i:i)\n        return\n      end if\n    end do\n  end function find_op\nend program\n",
        "f90",
    );
    let out = unique_path("fixed_char_out_dummy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("fixed char out dummy compile failed to spawn");
    assert!(
        compile.status.success(),
        "fixed char out dummy compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("fixed char out dummy run failed");
    assert!(
        run.status.success(),
        "fixed char out dummy run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains('2') && stdout.contains('*'),
        "fixed char out dummy should write back operator: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_component_substring_result_keeps_dynamic_upper_bound() {
    let src = write_program(
        "module m\n  use iso_c_binding, only: c_int\n  implicit none\n  interface\n    subroutine c_exit(code) bind(C, name='exit')\n      import :: c_int\n      integer(c_int), value :: code\n    end subroutine\n  end interface\n  type :: var_t\n    character(len=32) :: name = ''\n    character(len=:), allocatable :: value\n    integer :: value_len = 0\n  end type\n  type :: shell_t\n    type(var_t) :: variables(4)\n  end type\ncontains\n  function get_var(shell, name) result(v)\n    type(shell_t), intent(in) :: shell\n    character(len=*), intent(in) :: name\n    character(len=:), allocatable :: v\n    v = ''\n    if (trim(shell%variables(1)%name) == trim(name)) then\n      if (shell%variables(1)%value_len > 0) then\n        v = shell%variables(1)%value(1:shell%variables(1)%value_len)\n      end if\n    end if\n  end function\nend module\n\nprogram p\n  use m\n  implicit none\n  type(shell_t) :: shell\n  character(len=:), allocatable :: v\n  shell%variables(1)%name = 'a'\n  shell%variables(1)%value = '10'\n  shell%variables(1)%value_len = 2\n  v = get_var(shell, 'a')\n  if (trim(v) /= '10') call c_exit(3_c_int)\n  call c_exit(0_c_int)\nend program\n",
        "f90",
    );
    let out = unique_path("alloc_component_substring_result", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable component substring result compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocatable component substring result compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("allocatable component substring result run failed");
    assert!(
        run.status.success(),
        "allocatable component substring result run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_character_component_descriptor_starts_zeroed() {
    let src = write_program(
        "module m\n  use iso_c_binding, only: c_int\n  implicit none\n  interface\n    subroutine c_exit(code) bind(C, name='exit')\n      import :: c_int\n      integer(c_int), value :: code\n    end subroutine\n  end interface\n  type :: var_t\n    character(len=:), allocatable :: value\n  end type\n  type :: shell_t\n    type(var_t) :: vars(4)\n  end type\nend module\n\nprogram p\n  use m\n  implicit none\n  type(shell_t) :: shell\n  shell%vars(1)%value = '10'\n  if (.not. allocated(shell%vars(1)%value)) call c_exit(1_c_int)\n  deallocate(shell%vars(1)%value)\n  call c_exit(0_c_int)\nend program\n",
        "f90",
    );
    let out = unique_path("alloc_component_zero_init", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable character component zero-init compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocatable character component zero-init compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("allocatable character component zero-init run failed");
    assert!(
        run.status.success(),
        "allocatable character component zero-init run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_character_component_update_through_inout_dummy_runs() {
    let src = write_program(
        "module m\n  use iso_c_binding, only: c_int\n  implicit none\n  interface\n    subroutine c_exit(code) bind(C, name='exit')\n      import :: c_int\n      integer(c_int), value :: code\n    end subroutine\n  end interface\n  type :: var_t\n    character(len=:), allocatable :: value\n    integer :: value_len = 0\n  end type\n  type :: shell_t\n    type(var_t) :: vars(4)\n  end type\ncontains\n  subroutine safe_assign_alloc_str(dest, src, src_len)\n    character(len=:), allocatable, intent(inout) :: dest\n    character(len=*), intent(in) :: src\n    integer, intent(in) :: src_len\n    integer :: k\n    if (allocated(dest)) deallocate(dest)\n    if (src_len <= 0) then\n      allocate(character(len=0) :: dest)\n      return\n    end if\n    allocate(character(len=src_len) :: dest)\n    do k = 1, src_len\n      dest(k:k) = src(k:k)\n    end do\n  end subroutine\nend module\n\nprogram p\n  use m\n  implicit none\n  type(shell_t) :: shell\n  shell%vars(1)%value = '10'\n  shell%vars(1)%value_len = 2\n  call safe_assign_alloc_str(shell%vars(1)%value, '20', 2)\n  if (trim(shell%vars(1)%value) /= '20') call c_exit(1_c_int)\n  call c_exit(0_c_int)\nend program\n",
        "f90",
    );
    let out = unique_path("alloc_component_update_inout", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable character component update compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocatable character component update compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("allocatable character component update run failed");
    assert!(
        run.status.success(),
        "allocatable character component update run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn bind_c_name_call_uses_declared_c_symbol() {
    let src = write_program(
        "program p\n  use iso_c_binding, only: c_int\n  implicit none\n  interface\n    function getpid_c() bind(c, name='getpid') result(pid)\n      import :: c_int\n      integer(c_int) :: pid\n    end function getpid_c\n  end interface\n  integer(c_int) :: pid\n  pid = getpid_c()\nend program\n",
        "f90",
    );
    let out = unique_path("bind_c_name_call", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("bind(c) name compile failed to spawn");
    assert!(
        compile.status.success(),
        "bind(c) name compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&out);
    assert!(
        undefined.iter().any(|sym| sym == "_getpid"),
        "bind(c, name=...) should call the declared C symbol: {:?}",
        undefined
    );
    assert!(
        !undefined.iter().any(|sym| sym == "_getpid_c"),
        "bind(c, name=...) should not call the local Fortran alias: {:?}",
        undefined
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn bind_c_subroutine_value_arg_is_passed_by_value() {
    let dir = unique_dir("bind_c_subroutine_value");
    let c_src = write_program_in(
        &dir,
        "store_incremented.c",
        "#include <stdint.h>\n\nvoid store_incremented(int32_t value, int32_t *out) {\n    *out = value + 1;\n}\n",
    );
    let c_obj = dir.join("store_incremented.o");
    compile_c_object(&c_src, &c_obj);

    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_int\n  implicit none\n  interface\n    subroutine store_incremented(value, out) bind(C, name='store_incremented')\n      import :: c_int\n      integer(c_int), value :: value\n      integer(c_int), intent(out) :: out\n    end subroutine store_incremented\n  end interface\n  integer(c_int) :: out\n  call store_incremented(41_c_int, out)\n  if (out /= 42_c_int) error stop 1\n  print *, out\nend program\n",
    );

    let main_obj = dir.join("main.o");
    let compile_obj = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) value subroutine object compile failed to spawn");
    assert!(
        compile_obj.status.success(),
        "bind(c) value subroutine should compile to an object: {}",
        String::from_utf8_lossy(&compile_obj.stderr)
    );

    let exe = dir.join("bind_c_subroutine_value.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            main_obj.to_str().unwrap(),
            c_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) value subroutine link failed to spawn");
    assert!(
        link.status.success(),
        "bind(c) value subroutine objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("bind(c) value run failed");
    assert!(
        run.status.success(),
        "bind(c) value subroutine should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("42"),
        "bind(c) value subroutine should observe the by-value integer argument: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bind_c_interface_subroutine_value_survives_amod_import_and_runs() {
    let dir = unique_dir("bind_c_interface_value_amod");
    let c_src = write_program_in(
        &dir,
        "store_incremented.c",
        "#include <stdint.h>\n\nvoid store_incremented(int32_t value, int32_t *out) {\n    *out = value + 1;\n}\n",
    );
    let c_obj = dir.join("store_incremented.o");
    compile_c_object(&c_src, &c_obj);

    let mod_src = write_program_in(
        &dir,
        "c_math.f90",
        "module c_math\n  use iso_c_binding, only: c_int\n  implicit none\n  interface\n    subroutine store_incremented(value, out) bind(C, name='store_incremented')\n      import :: c_int\n      integer(c_int), value :: value\n      integer(c_int), intent(out) :: out\n    end subroutine store_incremented\n  end interface\nend module c_math\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_int\n  use c_math, only: store_incremented\n  implicit none\n  integer(c_int) :: out\n  call store_incremented(41_c_int, out)\n  if (out /= 42_c_int) error stop 1\n  print *, out\nend program\n",
    );

    let mod_obj = dir.join("c_math.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) interface module compile failed to spawn");
    assert!(
        compile_mod.status.success(),
        "bind(c) interface module should compile: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let amod = std::fs::read_to_string(dir.join("c_math.amod")).expect("missing c_math.amod");
    assert!(
        amod.contains("@arg value") && amod.contains("value"),
        "interface-declared VALUE arg should survive into .amod: {}",
        amod
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) interface user compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "bind(c) interface user should compile: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("bind_c_interface_value.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            main_obj.to_str().unwrap(),
            c_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) interface user link failed to spawn");
    assert!(
        link.status.success(),
        "bind(c) interface user objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("bind(c) interface user run failed");
    assert!(
        run.status.success(),
        "bind(c) interface user binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("42"),
        "unexpected bind(c) interface user output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bind_c_c_char_buffer_writes_scalar_character_storage() {
    let dir = unique_dir("bind_c_c_char_buffer");
    let c_src = write_program_in(
        &dir,
        "fill_chars.c",
        "#include <stddef.h>\n\nsize_t fill_chars(char *buf, size_t n) {\n    static const char msg[] = \"hello world\";\n    size_t len = sizeof(msg) - 1;\n    if (n < len) len = n;\n    for (size_t i = 0; i < len; ++i) buf[i] = msg[i];\n    return len;\n}\n",
    );
    let c_obj = dir.join("fill_chars.o");
    compile_c_object(&c_src, &c_obj);

    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_char, c_size_t\n  implicit none\n  interface\n    function fill_chars(buf, n) result(copied) bind(C, name='fill_chars')\n      import :: c_char, c_size_t\n      character(kind=c_char) :: buf(*)\n      integer(c_size_t), value :: n\n      integer(c_size_t) :: copied\n    end function\n  end interface\n  character(len=11) :: fixed\n  character(len=:), allocatable :: dyn\n  integer(c_size_t) :: copied\n\n  fixed = '           '\n  copied = fill_chars(fixed, int(len(fixed), c_size_t))\n  if (fixed /= 'hello world') error stop 1\n\n  allocate(character(len=11) :: dyn)\n  dyn = '           '\n  copied = fill_chars(dyn, int(len(dyn), c_size_t))\n  if (dyn /= 'hello world') error stop 2\n  if (copied /= int(11, c_size_t)) error stop 3\n\n  print *, trim(fixed)\n  print *, trim(dyn)\nend program\n",
    );

    let main_obj = dir.join("main.o");
    let compile_obj = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) c_char buffer object compile failed to spawn");
    assert!(
        compile_obj.status.success(),
        "bind(c) c_char buffer should compile to an object: {}",
        String::from_utf8_lossy(&compile_obj.stderr)
    );

    let exe = dir.join("bind_c_c_char_buffer.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            main_obj.to_str().unwrap(),
            c_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) c_char buffer link failed to spawn");
    assert!(
        link.status.success(),
        "bind(c) c_char buffer objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("bind(c) c_char buffer run failed");
    assert!(
        run.status.success(),
        "bind(c) c_char buffer should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.matches("hello world").count() >= 2,
        "bind(c) c_char buffer should update both scalar character actuals: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bind_c_c_char_buffer_survives_amod_import_without_hidden_lengths() {
    let dir = unique_dir("bind_c_c_char_buffer_amod");
    let c_src = write_program_in(
        &dir,
        "fill_chars.c",
        "#include <stddef.h>\n\nsize_t fill_chars(char *buf, size_t n) {\n    static const char msg[] = \"hello world\";\n    size_t len = sizeof(msg) - 1;\n    if (n < len) len = n;\n    for (size_t i = 0; i < len; ++i) buf[i] = msg[i];\n    return len;\n}\n",
    );
    let c_obj = dir.join("fill_chars.o");
    compile_c_object(&c_src, &c_obj);

    let mod_src = write_program_in(
        &dir,
        "c_strings.f90",
        "module c_strings\n  use iso_c_binding, only: c_char, c_size_t\n  implicit none\n  interface\n    function fill_chars(buf, n) result(copied) bind(C, name='fill_chars')\n      import :: c_char, c_size_t\n      character(kind=c_char) :: buf(*)\n      integer(c_size_t), value :: n\n      integer(c_size_t) :: copied\n    end function\n  end interface\nend module c_strings\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_size_t\n  use c_strings, only: fill_chars\n  implicit none\n  character(len=11) :: fixed\n  integer(c_size_t) :: copied\n\n  fixed = '           '\n  copied = fill_chars(fixed, int(len(fixed), c_size_t))\n  if (fixed /= 'hello world') error stop 1\n  if (copied /= int(11, c_size_t)) error stop 2\n  print *, trim(fixed)\nend program\n",
    );

    let mod_obj = dir.join("c_strings.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) c_char module compile failed to spawn");
    assert!(
        compile_mod.status.success(),
        "bind(c) c_char interface module should compile: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let amod = std::fs::read_to_string(dir.join("c_strings.amod")).expect("missing c_strings.amod");
    assert!(
        amod.contains("@abi cc=aapcs64 hidden_char_lens=0"),
        "bind(c) c_char buffer interface should not advertise hidden lengths: {}",
        amod
    );
    assert!(
        !amod.contains("@arg buf@len"),
        "bind(c) c_char buffer interface should not serialize hidden len args: {}",
        amod
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) c_char interface user compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "bind(c) c_char interface user should compile: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("bind_c_c_char_buffer_amod.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            main_obj.to_str().unwrap(),
            c_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) c_char interface user link failed to spawn");
    assert!(
        link.status.success(),
        "bind(c) c_char interface user objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("bind(c) c_char interface user run failed");
    assert!(
        run.status.success(),
        "bind(c) c_char interface user binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("hello world"),
        "bind(c) c_char buffer should survive .amod import and still write the caller storage: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bind_c_c_char_value_arg_passes_actual_byte_after_value_handle() {
    let dir = unique_dir("bind_c_c_char_value_arg");
    let c_src = write_program_in(
        &dir,
        "check_char.c",
        "#include <stddef.h>\n\nint check_char(void *handle, char ch) {\n    (void)handle;\n    return (unsigned char)ch;\n}\n",
    );
    let c_obj = dir.join("check_char.o");
    compile_c_object(&c_src, &c_obj);

    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_ptr, c_null_ptr, c_char, c_int\n  implicit none\n  interface\n    function check_char(handle, ch) result(rc) bind(C, name='check_char')\n      import :: c_ptr, c_char, c_int\n      type(c_ptr), value :: handle\n      character(kind=c_char), value :: ch\n      integer(c_int) :: rc\n    end function\n  end interface\n  character(len=3) :: s\n  integer(c_int) :: rc\n\n  s = ' +0'\n\n  rc = check_char(c_null_ptr, ' ')\n  if (rc /= 32) error stop 1\n  rc = check_char(c_null_ptr, '+')\n  if (rc /= 43) error stop 2\n  rc = check_char(c_null_ptr, s(1:1))\n  if (rc /= 32) error stop 3\n  rc = check_char(c_null_ptr, s(2:2))\n  if (rc /= 43) error stop 4\n  rc = check_char(c_null_ptr, s(3:3))\n  if (rc /= 48) error stop 5\n\n  print *, 'ok'\nend program\n",
    );

    let main_obj = dir.join("main.o");
    let compile_obj = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) c_char value object compile failed to spawn");
    assert!(
        compile_obj.status.success(),
        "bind(c) c_char value object should compile: {}",
        String::from_utf8_lossy(&compile_obj.stderr)
    );

    let exe = dir.join("bind_c_c_char_value_arg.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            main_obj.to_str().unwrap(),
            c_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) c_char value link failed to spawn");
    assert!(
        link.status.success(),
        "bind(c) c_char value objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("bind(c) c_char value run failed");
    assert!(
        run.status.success(),
        "bind(c) c_char value should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "bind(c) c_char value should pass the actual byte for literals and substrings: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bind_c_interface_function_returning_c_ptr_runs() {
    let dir = unique_dir("bind_c_c_ptr_return");
    let c_src = write_program_in(
        &dir,
        "get_static_buf.c",
        "#include <stddef.h>\n\nvoid *get_static_buf(void) {\n    static char buf[4] = {'o', 'k', 0, 0};\n    return buf;\n}\n",
    );
    let c_obj = dir.join("get_static_buf.o");
    compile_c_object(&c_src, &c_obj);

    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_ptr, c_char, c_f_pointer\n  implicit none\n  interface\n    function get_static_buf() result(raw) bind(C, name='get_static_buf')\n      import :: c_ptr\n      type(c_ptr) :: raw\n    end function\n  end interface\n  type(c_ptr) :: raw\n  character(kind=c_char), pointer :: view(:)\n\n  raw = get_static_buf()\n  call c_f_pointer(raw, view, [4])\n  if (.not. associated(view)) error stop 1\n  if (view(1) /= achar(111, kind=c_char)) error stop 2\n  if (view(2) /= achar(107, kind=c_char)) error stop 3\n  print *, 'ok'\nend program\n",
    );

    let main_obj = dir.join("main.o");
    let compile_obj = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) c_ptr return object compile failed to spawn");
    assert!(
        compile_obj.status.success(),
        "bind(c) c_ptr return should compile to an object: {}",
        String::from_utf8_lossy(&compile_obj.stderr)
    );

    let exe = dir.join("bind_c_c_ptr_return.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            main_obj.to_str().unwrap(),
            c_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) c_ptr return link failed to spawn");
    assert!(
        link.status.success(),
        "bind(c) c_ptr return objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("bind(c) c_ptr return run failed");
    assert!(
        run.status.success(),
        "bind(c) c_ptr return should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "bind(c) c_ptr return should preserve the full pointer value: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn module_procedure_case_and_bind_label_survive_amod_import() {
    let dir = unique_dir("amod_case_bind");
    let mod_src = write_program_in(
        &dir,
        "m.f90",
        "module m\n  use iso_c_binding, only: c_int\n  implicit none\n  interface\n    function C_CLOSE(fd) bind(c, name='close') result(ret)\n      import :: c_int\n      integer(c_int), value :: fd\n      integer(c_int) :: ret\n    end function C_CLOSE\n  end interface\ncontains\n  function WEXITSTATUS(status) result(exit_status)\n    integer(c_int), intent(in) :: status\n    integer :: exit_status\n    exit_status = status + 1\n  end function WEXITSTATUS\nend module\n",
    );
    let use_src = write_program_in(
        &dir,
        "use_m.f90",
        "program p\n  use iso_c_binding, only: c_int\n  use m\n  implicit none\n  integer(c_int) :: status, closed\n  status = WEXITSTATUS(1_c_int)\n  closed = C_CLOSE(0_c_int)\nend program\n",
    );

    let mod_obj = dir.join("m.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .env("NO_COLOR", "1")
        .output()
        .expect("module compile failed to spawn");
    assert!(
        compile_mod.status.success(),
        "module compile failed: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let use_obj = dir.join("use_m.o");
    let compile_use = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            use_src.to_str().unwrap(),
            "-o",
            use_obj.to_str().unwrap(),
        ])
        .env("NO_COLOR", "1")
        .output()
        .expect("consumer compile failed to spawn");
    assert!(
        compile_use.status.success(),
        "consumer compile failed: {}",
        String::from_utf8_lossy(&compile_use.stderr)
    );

    let undefined = undefined_symbols(&use_obj);
    assert!(
        undefined
            .iter()
            .any(|sym| sym == "_afs_modproc_m_WEXITSTATUS"),
        "mixed-case module procedures should retain case across .amod import: {:?}",
        undefined
    );
    assert!(
        !undefined
            .iter()
            .any(|sym| sym == "_afs_modproc_m_wexitstatus"),
        "imported mixed-case module procedures should not be downcased: {:?}",
        undefined
    );
    assert!(
        undefined.iter().any(|sym| sym == "_close"),
        "bind(c, name=...) procedures should keep binding labels across .amod import: {:?}",
        undefined
    );
    assert!(
        !undefined.iter().any(|sym| sym == "_c_close"),
        "bind(c, name=...) procedures should not fall back to Fortran aliases: {:?}",
        undefined
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn repeat_intrinsic_lowers_to_runtime_symbol() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=:), allocatable :: s\n  s = repeat('ab', 3)\n  print *, len_trim(s)\nend program\n",
        "f90",
    );
    let out = unique_path("repeat_runtime", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("repeat intrinsic compile failed to spawn");
    assert!(
        compile.status.success(),
        "repeat intrinsic compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&out);
    assert!(
        undefined.iter().any(|sym| sym == "_afs_repeat"),
        "repeat intrinsic should lower to afs_repeat, undefineds were: {:?}",
        undefined
    );
    assert!(
        !undefined.iter().any(|sym| sym == "_repeat"),
        "repeat intrinsic should not lower to a raw repeat symbol: {:?}",
        undefined
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn pointer_dummy_associated_lowers_without_raw_symbol() {
    let src = write_program(
        "module m\n  implicit none\n  type :: node_t\n    integer :: value = 0\n  end type node_t\ncontains\n  logical function present(node) result(ok)\n    type(node_t), pointer, intent(in) :: node\n    ok = associated(node)\n  end function present\nend module m\n",
        "f90",
    );
    let out = unique_path("associated_pointer_dummy", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("pointer associated compile failed to spawn");
    assert!(
        compile.status.success(),
        "pointer associated compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&out);
    assert!(
        !undefined.iter().any(|sym| sym == "_associated"),
        "pointer dummy associated() should not escape as a raw symbol: {:?}",
        undefined
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn local_derived_pointer_actual_passes_target_to_pointer_dummy() {
    let src = write_program(
        "program p\n  implicit none\n  type :: node_t\n    integer :: value = 0\n  end type node_t\n  type(node_t), target :: target_node\n  type(node_t), pointer :: root\n  target_node%value = 42\n  root => target_node\n  call check(root)\ncontains\n  subroutine check(node)\n    type(node_t), pointer, intent(in) :: node\n    if (.not. associated(node)) error stop 1\n    if (node%value /= 42) error stop 2\n    print *, 'ok'\n  end subroutine check\nend program p\n",
        "f90",
    );
    let out = unique_path("derived_pointer_actual_to_pointer_dummy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived pointer actual compile failed to spawn");
    assert!(
        compile.status.success(),
        "derived pointer actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("derived pointer actual run failed");
    assert!(
        run.status.success(),
        "derived pointer actual run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected derived pointer actual output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn pointer_function_result_associated_lowers_without_raw_symbol() {
    let src = write_program(
        "module m\n  implicit none\n  type :: node_t\n    integer :: value = 0\n  end type node_t\ncontains\n  recursive function parse() result(node)\n    type(node_t), pointer :: node, right_node\n    nullify(node)\n    if (.not. associated(node)) return\n    if (.not. associated(right_node)) return\n  end function parse\nend module m\n",
        "f90",
    );
    let out = unique_path("associated_pointer_result", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("pointer result associated compile failed to spawn");
    assert!(
        compile.status.success(),
        "pointer result associated compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&out);
    assert!(
        !undefined.iter().any(|sym| sym == "_associated"),
        "pointer function-result associated() should not escape as a raw symbol: {:?}",
        undefined
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn component_array_intrinsics_survive_logical_condition_lowering() {
    let src = write_program(
        "module m\n  implicit none\n  type :: cmd_t\n    character(:), allocatable :: tokens(:)\n    integer, allocatable :: token_lengths(:)\n  end type cmd_t\ncontains\n  integer function f(cmd, i) result(strip_len)\n    type(cmd_t), intent(in) :: cmd\n    integer, intent(in) :: i\n    if (allocated(cmd%token_lengths) .and. i <= size(cmd%token_lengths) .and. cmd%token_lengths(i) > 0) then\n      strip_len = cmd%token_lengths(i)\n    else\n      strip_len = len_trim(cmd%tokens(i))\n    end if\n  end function f\nend module m\n",
        "f90",
    );
    let out = unique_path("component_array_condition", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("component array condition compile failed to spawn");
    assert!(
        compile.status.success(),
        "component array condition compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&out);
    assert!(
        undefined.iter().any(|sym| sym == "_afs_array_allocated"),
        "component array condition should lower allocated() to afs_array_allocated: {:?}",
        undefined
    );
    assert!(
        undefined.iter().any(|sym| sym == "_afs_array_size"),
        "component array condition should lower size() to afs_array_size: {:?}",
        undefined
    );
    assert!(
        undefined.iter().any(|sym| sym == "_afs_len_trim"),
        "component array condition should lower len_trim() to afs_len_trim: {:?}",
        undefined
    );
    assert!(
        !undefined
            .iter()
            .any(|sym| sym == "_allocated" || sym == "_size"),
        "component array condition should not call raw allocated/size symbols: {:?}",
        undefined
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_array_element_component_intrinsics_do_not_escape() {
    let src = write_program(
        "module m\n  implicit none\n  integer, parameter :: max_token_len = 32\n  type :: command_t\n    character(len=:), allocatable :: tokens(:)\n    character(len=max_token_len), allocatable :: prefix_assignments(:)\n    character(len=:), allocatable :: heredoc_delimiter\n  end type command_t\ncontains\n  subroutine f()\n    type(command_t), allocatable :: temp_commands(:)\n    integer :: i\n    allocate(temp_commands(2))\n    i = 1\n    if (allocated(temp_commands(i)%prefix_assignments)) print *, 1\n    if (allocated(temp_commands(i)%tokens)) print *, 2\n    if (allocated(temp_commands(i)%heredoc_delimiter)) print *, 3\n  end subroutine f\nend module m\n",
        "f90",
    );
    let out = unique_path("allocatable_base_component_intrinsics", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("allocatable base component intrinsic compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocatable base component intrinsic compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&out);
    assert!(
        undefined.iter().any(|sym| sym == "_afs_array_allocated"),
        "allocatable component arrays should lower allocated() to afs_array_allocated: {:?}",
        undefined
    );
    assert!(
        undefined.iter().any(|sym| sym == "_afs_string_allocated"),
        "allocatable character components should lower allocated() to afs_string_allocated: {:?}",
        undefined
    );
    assert!(
        !undefined.iter().any(|sym| sym == "_allocated"),
        "allocatable array-element component allocated() should not escape as a raw symbol: {:?}",
        undefined
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fixed_component_array_size_lowers_without_raw_symbol() {
    let src = write_program(
        "module m\n  implicit none\n  type :: shell_t\n    integer :: vars(4)\n  end type shell_t\ncontains\n  integer function f(shell) result(n)\n    type(shell_t), intent(in) :: shell\n    n = size(shell%vars)\n  end function f\nend module m\n",
        "f90",
    );
    let out = unique_path("fixed_component_size", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("fixed component size compile failed to spawn");
    assert!(
        compile.status.success(),
        "fixed component size compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&out);
    assert!(
        !undefined.iter().any(|sym| sym == "_size"),
        "fixed-size component array SIZE() should not escape as a raw symbol: {:?}",
        undefined
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocate_bounds_size_intrinsic_lowers_without_raw_symbol() {
    let src = write_program(
        "module m\n  implicit none\n  type :: string_t\n    character(:), allocatable :: str\n  end type string_t\n  type :: shell_t\n    type(string_t), allocatable :: positional_params(:)\n  end type shell_t\ncontains\n  subroutine f(shell)\n    type(shell_t), intent(inout) :: shell\n    type(string_t), allocatable :: saved(:)\n    integer :: i\n    if (allocated(shell%positional_params)) then\n      allocate(saved(size(shell%positional_params)))\n      do i = 1, size(shell%positional_params)\n        saved(i)%str = shell%positional_params(i)%str\n      end do\n    end if\n  end subroutine f\nend module m\n",
        "f90",
    );
    let out = unique_path("allocate_bounds_size", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("allocate-bounds size compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocate-bounds size compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&out);
    assert!(
        undefined.iter().any(|sym| sym == "_afs_array_size"),
        "allocate bounds should still lower size() to afs_array_size: {:?}",
        undefined
    );
    assert!(
        !undefined.iter().any(|sym| sym == "_size"),
        "allocate bounds size() should not escape as a raw symbol: {:?}",
        undefined
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fixed_component_array_element_assignment_compiles() {
    let src = write_program(
        "module m\n  implicit none\n  type :: command_t\n    integer :: code = 0\n  end type command_t\n  type :: trap_table_t\n    type(command_t) :: commands(3)\n  end type trap_table_t\ncontains\n  subroutine set_code(tab, i, v)\n    type(trap_table_t), intent(inout) :: tab\n    integer, intent(in) :: i, v\n    tab%commands(i)%code = v\n  end subroutine set_code\nend module m\n",
        "f90",
    );
    let out = unique_path("fixed_component_array", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("fixed component array compile failed to spawn");
    assert!(
        compile.status.success(),
        "fixed component array compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn scalar_char_component_ops_and_achar_compile() {
    let src = write_program(
        "module m\n  implicit none\n  type :: shell_t\n    character(len=8) :: ifs = ''\n  end type shell_t\ncontains\n  subroutine f(shell, sep)\n    type(shell_t), intent(in) :: shell\n    character(len=1), intent(out) :: sep\n    if (len_trim(shell%ifs) > 0) then\n      sep = shell%ifs(1:1)\n    else\n      sep = achar(0)\n    end if\n  end subroutine f\nend module m\n",
        "f90",
    );
    let out = unique_path("scalar_char_component", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("scalar char component compile failed to spawn");
    assert!(
        compile.status.success(),
        "scalar char component compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&out);
    assert!(
        undefined.iter().any(|sym| sym == "_afs_len_trim"),
        "scalar char component should lower len_trim() to afs_len_trim: {:?}",
        undefined
    );
    assert!(
        undefined.iter().any(|sym| sym == "_afs_char"),
        "ACHAR should lower to afs_char: {:?}",
        undefined
    );
    assert!(
        !undefined.iter().any(|sym| sym == "_achar" || sym == "_ifs"),
        "scalar char component lowering should not introduce raw achar/ifs symbols: {:?}",
        undefined
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocated_derived_pointer_preserves_blank_char_component_default() {
    let src = write_program(
        "program p\n  implicit none\n  type :: simple_command_data_t\n    character(len=8) :: heredoc_delimiter = ''\n  end type simple_command_data_t\n  type(simple_command_data_t), pointer :: cmd\n  allocate(cmd)\n  if (len_trim(cmd%heredoc_delimiter) /= 0) error stop 1\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("derived_pointer_blank_char_default", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived pointer blank-char default compile failed to spawn");
    assert!(
        compile.status.success(),
        "derived pointer blank-char default compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("derived pointer blank-char default run failed");
    assert!(
        run.status.success(),
        "derived pointer blank-char default run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocated_pointer_component_preserves_blank_char_component_default() {
    let src = write_program(
        "program p\n  implicit none\n  type :: simple_command_data_t\n    character(len=:), allocatable :: words(:)\n    integer, allocatable :: word_lengths(:)\n    integer :: num_words = 0\n    character(len=256) :: heredoc_delimiter = ''\n    logical :: heredoc_quoted = .false.\n  end type simple_command_data_t\n  type :: command_node_t\n    type(simple_command_data_t), pointer :: simple_cmd => null()\n  end type command_node_t\n  type(command_node_t), pointer :: node\n  node => create_simple_command()\n  if (.not. associated(node%simple_cmd)) error stop 2\n  if (len_trim(node%simple_cmd%heredoc_delimiter) /= 0) error stop 1\n  print *, 'ok'\ncontains\n  function create_simple_command() result(node)\n    type(command_node_t), pointer :: node\n    allocate(node)\n    allocate(node%simple_cmd)\n    allocate(character(len=32) :: node%simple_cmd%words(1))\n    node%simple_cmd%words(1) = 'false'\n  end function create_simple_command\nend program p\n",
        "f90",
    );
    let out = unique_path("pointer_component_blank_char_default", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("pointer component blank-char default compile failed to spawn");
    assert!(
        compile.status.success(),
        "pointer component blank-char default compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("pointer component blank-char default run failed");
    assert!(
        run.status.success(),
        "pointer component blank-char default run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn scalar_pointer_component_assignment_and_read_round_trip() {
    let src = write_program(
        "program p\n  implicit none\n  type :: box_t\n    integer, pointer :: p => null()\n  end type\n  type(box_t) :: box\n  integer, target :: value\n  value = 11\n  box%p => value\n  print *, box%p\nend program p\n",
        "f90",
    );
    let out = unique_path("scalar_pointer_component_round_trip", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("scalar pointer component compile failed to spawn");
    assert!(
        compile.status.success(),
        "scalar pointer component compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("scalar pointer component run failed");
    assert!(
        run.status.success(),
        "scalar pointer component run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("11"),
        "unexpected scalar pointer component output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn nested_pointer_component_array_element_access_round_trips() {
    let src = write_program(
        "program p\n  implicit none\n  type :: cmd_t\n    integer :: x = 0\n  end type\n  type :: pipeline_t\n    type(cmd_t), pointer :: commands(:) => null()\n  end type\n  type :: node_t\n    type(pipeline_t), pointer :: pipeline => null()\n  end type\n  type(node_t) :: node\n  type(cmd_t), target :: backing(2)\n  backing(1)%x = 11\n  backing(2)%x = 22\n  allocate(node%pipeline)\n  node%pipeline%commands => backing\n  print *, node%pipeline%commands(1)%x, node%pipeline%commands(2)%x\nend program p\n",
        "f90",
    );
    let out = unique_path("nested_pointer_component_array", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("nested pointer component array compile failed to spawn");
    assert!(
        compile.status.success(),
        "nested pointer component array compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("nested pointer component array run failed");
    assert!(
        run.status.success(),
        "nested pointer component array run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("11") && stdout.contains("22"),
        "unexpected nested pointer component array output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn nested_scalar_derived_pointer_component_access_and_call_round_trip() {
    let src = write_program(
        "module m\n  implicit none\n  type :: list_t\n    type(node_t), pointer :: left => null()\n    type(node_t), pointer :: right => null()\n    integer :: sep = 0\n  end type\n  type :: node_t\n    integer :: kind = 0\n    type(list_t), pointer :: list => null()\n  end type\ncontains\n  function make_simple(v) result(node)\n    integer, intent(in) :: v\n    type(node_t), pointer :: node\n    allocate(node)\n    node%kind = v\n  end function\n\n  function make_list(left, right, sep) result(node)\n    type(node_t), pointer, intent(in) :: left, right\n    integer, intent(in) :: sep\n    type(node_t), pointer :: node\n    allocate(node)\n    node%kind = 99\n    allocate(node%list)\n    node%list%left => left\n    node%list%right => right\n    node%list%sep = sep\n  end function\n\n  function read_kind(node) result(v)\n    type(node_t), pointer, intent(in) :: node\n    integer :: v\n    if (associated(node)) then\n      v = node%kind\n    else\n      v = -1\n    end if\n  end function\nend module\n\nprogram main\n  use m\n  implicit none\n  type(node_t), pointer :: root\n  root => make_list(make_simple(11), make_simple(22), 7)\n  print *, root%list%left%kind\n  print *, read_kind(root%list%left)\n  print *, read_kind(root%list%right)\nend program\n",
        "f90",
    );
    let out = unique_path("nested_scalar_derived_pointer_component", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("nested scalar derived pointer component compile failed to spawn");
    assert!(
        compile.status.success(),
        "nested scalar derived pointer component compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("nested scalar derived pointer component run failed");
    assert!(
        run.status.success(),
        "nested scalar derived pointer component run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("11") && stdout.contains("22"),
        "unexpected nested scalar derived pointer component output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn pointer_result_self_argument_survives_module_boundary() {
    let dir = unique_dir("pointer_result_self_argument");
    let tree_src = write_program_in(
        &dir,
        "tree.f90",
        "module tree\n  implicit none\n  integer, parameter :: NODE_SIMPLE = 1, NODE_LIST = 2\n  type :: list_t\n    type(node_t), pointer :: left => null()\n    type(node_t), pointer :: right => null()\n    integer :: sep = 0\n  end type\n  type :: node_t\n    integer :: node_type = 0\n    integer :: line = 0\n    integer :: column = 0\n    type(list_t), pointer :: list => null()\n    integer, allocatable :: redirects(:)\n    integer :: num_redirects = 0\n  end type\ncontains\n  function create_simple(kind) result(node)\n    integer, intent(in) :: kind\n    type(node_t), pointer :: node\n    allocate(node)\n    node%node_type = kind\n  end function\n  function create_list(left, right, sep) result(node)\n    type(node_t), pointer, intent(in) :: left, right\n    integer, intent(in) :: sep\n    type(node_t), pointer :: node\n    allocate(node)\n    node%node_type = NODE_LIST\n    allocate(node%list)\n    node%list%left => left\n    node%list%right => right\n    node%list%sep = sep\n  end function\nend module\n",
    );
    let builder_src = write_program_in(
        &dir,
        "builder.f90",
        "module builder\n  use tree\n  implicit none\ncontains\n  function make_root() result(node)\n    type(node_t), pointer :: node\n    type(node_t), pointer :: right\n    node => create_simple(11)\n    right => create_simple(22)\n    node => create_list(node, right, 1)\n  end function\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program main\n  use builder\n  use tree\n  implicit none\n  type(node_t), pointer :: root\n  root => make_root()\n  print *, root%node_type\n  print *, root%list%sep\n  print *, root%list%left%node_type\n  print *, root%list%right%node_type\nend program\n",
    );
    let mod_dir = dir.join("mod");
    std::fs::create_dir_all(&mod_dir).expect("cannot create module directory");
    let tree_obj = dir.join("tree.o");
    let builder_obj = dir.join("builder.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("main.bin");

    let compile_tree = Command::new(compiler("armfortas"))
        .args([
            "-c",
            tree_src.to_str().unwrap(),
            "-J",
            mod_dir.to_str().unwrap(),
            "-o",
            tree_obj.to_str().unwrap(),
        ])
        .output()
        .expect("tree module compile failed to spawn");
    assert!(
        compile_tree.status.success(),
        "tree module compile failed: {}",
        String::from_utf8_lossy(&compile_tree.stderr)
    );

    let compile_builder = Command::new(compiler("armfortas"))
        .args([
            "-c",
            builder_src.to_str().unwrap(),
            "-J",
            mod_dir.to_str().unwrap(),
            "-I",
            mod_dir.to_str().unwrap(),
            "-o",
            builder_obj.to_str().unwrap(),
        ])
        .output()
        .expect("builder module compile failed to spawn");
    assert!(
        compile_builder.status.success(),
        "builder module compile failed: {}",
        String::from_utf8_lossy(&compile_builder.stderr)
    );

    let compile_main = Command::new(compiler("armfortas"))
        .args([
            "-c",
            main_src.to_str().unwrap(),
            "-J",
            mod_dir.to_str().unwrap(),
            "-I",
            mod_dir.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("main compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "main compile failed: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let link = Command::new(compiler("armfortas"))
        .args([
            main_obj.to_str().unwrap(),
            builder_obj.to_str().unwrap(),
            tree_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link failed to spawn");
    assert!(
        link.status.success(),
        "link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("self-argument module boundary run failed");
    assert!(
        run.status.success(),
        "self-argument module boundary run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("2")
            && stdout.contains("1")
            && stdout.contains("11")
            && stdout.contains("22"),
        "unexpected self-argument module boundary output: {}",
        stdout
    );
}

#[test]
fn pointer_component_null_assignment_and_default_do_not_escape_null_symbol() {
    let src = write_program(
        "program p\n  implicit none\n  type :: cmd_t\n    integer :: x = 0\n  end type\n  type :: entry_t\n    type(cmd_t), pointer :: body => null()\n  end type\n  type(entry_t) :: entry\n  if (associated(entry%body)) error stop 1\n  entry%body => null()\n  if (associated(entry%body)) error stop 2\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("pointer_component_null_assignment", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("pointer component null compile failed to spawn");
    assert!(
        compile.status.success(),
        "pointer component null compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("pointer component null run failed");
    assert!(
        run.status.success(),
        "pointer component null run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected pointer component null output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn nullified_pointer_component_actual_passes_slot_to_pointer_dummy() {
    let src = write_program(
        "module m\n  implicit none\n  type :: child_t\n    integer :: tag = 0\n  end type\n  type :: holder_t\n    type(child_t), pointer :: body => null()\n  end type\n  type :: node_t\n    type(holder_t), pointer :: fn => null()\n  end type\ncontains\n  subroutine check_child(n)\n    type(child_t), pointer, intent(inout) :: n\n    if (associated(n)) then\n      print *, 'ASSOC', n%tag\n    else\n      print *, 'NULL'\n    end if\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(child_t), pointer :: leaf\n  type(node_t), pointer :: parent\n  allocate(parent)\n  allocate(parent%fn)\n  allocate(leaf)\n  leaf%tag = 42\n  parent%fn%body => leaf\n  nullify(parent%fn%body)\n  call check_child(parent%fn%body)\n  print *, 'DONE'\nend program\n",
        "f90",
    );
    let out = unique_path("nullified_pointer_component_actual", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("nullified pointer component actual compile failed to spawn");
    assert!(
        compile.status.success(),
        "nullified pointer component actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("nullified pointer component actual run failed");
    assert!(
        run.status.success(),
        "nullified pointer component actual run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("NULL"),
        "pointer dummy should observe a nullified component actual as disassociated: {}",
        stdout
    );
    assert!(
        stdout.contains("DONE"),
        "program should continue after the pointer-dummy check: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn pointer_dummy_deallocate_and_nullify_write_back_to_actual_slot() {
    let src = write_program(
        "module m\n  implicit none\n  integer :: count = 0\n  type :: child_t\n    integer :: tag = 0\n  end type\ncontains\n  subroutine destroy_child(n)\n    type(child_t), pointer, intent(inout) :: n\n    if (.not. associated(n)) return\n    count = count + 1\n    deallocate(n)\n    nullify(n)\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(child_t), pointer :: cached\n  allocate(cached)\n  cached%tag = 42\n  call destroy_child(cached)\n  print *, 'COUNT', count\n  if (associated(cached)) then\n    print *, 'CACHED', cached%tag\n  else\n    print *, 'CACHED', -1\n  end if\nend program\n",
        "f90",
    );
    let out = unique_path("pointer_dummy_dealloc_writeback", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("pointer dummy deallocate compile failed to spawn");
    assert!(
        compile.status.success(),
        "pointer dummy deallocate compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("pointer dummy deallocate run failed");
    assert!(
        run.status.success(),
        "pointer dummy deallocate run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("COUNT 1"),
        "pointer dummy deallocate should run exactly once: {}",
        stdout
    );
    assert!(
        stdout.contains("CACHED -1"),
        "pointer dummy deallocate/nullify should disassociate the caller slot: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn deferred_char_component_allocate_source_copies_runtime_string_value() {
    let src = write_program(
        "program p\n  implicit none\n  type :: redirect_t\n    character(:), allocatable :: filename\n  end type redirect_t\n  type(redirect_t) :: redirects(1)\n  character(len=8) :: tok\n  tok = 'abc   '\n  allocate(redirects(1)%filename, source=trim(tok))\n  if (len(redirects(1)%filename) /= 3) error stop 1\n  if (redirects(1)%filename /= 'abc') error stop 2\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("deferred_char_component_source", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("deferred char component SOURCE= compile failed to spawn");
    assert!(
        compile.status.success(),
        "deferred char component SOURCE= compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("deferred char component SOURCE= run failed");
    assert!(
        run.status.success(),
        "deferred char component SOURCE= run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn scalar_char_substring_argument_avoids_raw_local_symbol() {
    let src = write_program(
        "module m\n  implicit none\ncontains\n  integer function visual_length(s)\n    character(len=*), intent(in) :: s\n    visual_length = len_trim(s)\n  end function visual_length\n\n  integer function run(input) result(n)\n    character(len=*), intent(in) :: input\n    character(len=len(input)) :: working_input\n    working_input = input\n    n = visual_length(working_input(2:3))\n  end function run\nend module m\n",
        "f90",
    );
    let out = unique_path("char_substring_arg", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("char substring argument compile failed to spawn");
    assert!(
        compile.status.success(),
        "char substring argument compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&out);
    assert!(
        undefined.iter().any(|sym| sym == "_afs_len_trim"),
        "character dummy call should still route len_trim through the runtime: {:?}",
        undefined
    );
    assert!(
        !undefined.iter().any(|sym| sym == "_working_input"),
        "character substring argument should not lower as an external local symbol: {:?}",
        undefined
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocated_on_derived_array_element_component_uses_descriptor_runtime() {
    let src = write_program(
        "program p\n  implicit none\n  type :: cmd_t\n    character(:), allocatable :: tokens(:)\n  end type cmd_t\n  type(cmd_t) :: cmds(2)\n  logical :: ok\n  ok = allocated(cmds(1)%tokens)\n  if (ok) print *, size(cmds(1)%tokens)\nend program\n",
        "f90",
    );
    let out = unique_path("derived_array_component_allocated", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived array component allocated compile failed to spawn");
    assert!(
        compile.status.success(),
        "derived array component allocated compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&out);
    assert!(
        undefined.iter().any(|sym| sym == "_afs_array_allocated"),
        "allocated(cmds(i)%tokens) should lower to afs_array_allocated: {:?}",
        undefined
    );
    assert!(
        undefined.iter().any(|sym| sym == "_afs_array_size"),
        "size(cmds(i)%tokens) should lower to afs_array_size: {:?}",
        undefined
    );
    assert!(
        !undefined.iter().any(|sym| sym == "_allocated" || sym == "_size"),
        "derived array element component intrinsics should not call raw allocated/size symbols: {:?}",
        undefined
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_derived_shell_initialization_runs_through_components() {
    let src = write_program(
        "program p\n  implicit none\n  type :: string_t\n    character(:), allocatable :: str\n  end type string_t\n  type :: shell_t\n    type(string_t), allocatable :: positional_params(:)\n    integer, allocatable :: counts(:)\n  end type shell_t\n  type(shell_t), allocatable :: shell\n  allocate(shell)\n  call initialize_shell(shell)\n  if (.not. allocated(shell%positional_params)) stop 10\n  if (.not. allocated(shell%counts)) stop 11\n  if (shell%counts(1) /= 7) stop 12\n  print *, trim(shell%positional_params(1)%str)\ncontains\n  subroutine initialize_shell(shell)\n    type(shell_t), intent(out) :: shell\n    if (allocated(shell%positional_params)) stop 1\n    if (allocated(shell%counts)) stop 2\n    allocate(shell%positional_params(2))\n    allocate(shell%counts(2))\n    shell%positional_params(1)%str = 'ok'\n    shell%counts = [7, 9]\n  end subroutine initialize_shell\nend program\n",
        "f90",
    );
    let out = unique_path("allocatable_derived_shell", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable derived shell compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocatable derived shell compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("allocatable derived shell run failed");
    assert!(
        run.status.success(),
        "allocatable derived shell run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "allocatable derived shell should initialize nested components: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_scalar_derived_type_preserves_field_defaults_on_allocate() {
    let src = write_program(
        "program p\n  implicit none\n  type :: shell_t\n    integer :: ifs_len = -1\n    integer :: other = 7\n  end type shell_t\n  type(shell_t), allocatable :: shell\n  allocate(shell)\n  if (shell%ifs_len /= -1) error stop 1\n  if (shell%other /= 7) error stop 2\n  print *, shell%ifs_len, shell%other\nend program\n",
        "f90",
    );
    let out = unique_path("alloc_scalar_derived_defaults", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable scalar derived defaults compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocatable scalar derived defaults compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("allocatable scalar derived defaults run failed");
    assert!(
        run.status.success(),
        "allocatable scalar derived defaults run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("-1") && stdout.contains("7"),
        "allocatable scalar derived defaults should survive allocate(): {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_shell_default_ifs_follows_trim_branch() {
    let src = write_program(
        "program p\n  implicit none\n  type :: shell_t\n    integer :: ifs_len = -1\n  end type shell_t\n  type(shell_t), allocatable :: shell\n  character(len=32) :: input_line\n  character(len=32) :: var\n  integer :: actual_input_len\n\n  allocate(shell)\n\n  input_line = 'hello\\\\world '\n  actual_input_len = 12\n  if (shell%ifs_len == 0) then\n    var = input_line(:actual_input_len)\n  else\n    var = trim(adjustl(input_line))\n  end if\n  if (trim(var) /= 'hello\\\\world') error stop 1\n\n  input_line = '  x  '\n  actual_input_len = 5\n  if (shell%ifs_len == 0) then\n    var = input_line(:actual_input_len)\n  else\n    var = trim(adjustl(input_line))\n  end if\n  if (trim(var) /= 'x') error stop 2\n\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("alloc_shell_default_ifs", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable shell default ifs compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocatable shell default ifs compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("allocatable shell default ifs run failed");
    assert!(
        run.status.success(),
        "allocatable shell default ifs run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "allocatable shell default ifs should take trim branch: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn derived_array_section_actual_preserves_bounds_and_elements() {
    let src = write_program(
        "program p\n  implicit none\n  type :: string_t\n    character(len=:), allocatable :: str\n  end type\n  type :: var_t\n    type(string_t), allocatable :: array_values(:)\n    integer :: array_size = 0\n  end type\n  type :: shell_t\n    type(var_t) :: variables(4)\n    integer :: num_variables = 0\n  end type\n  type(string_t), allocatable :: values(:)\n  type(shell_t) :: shell\n  integer :: count\n\n  allocate(values(20))\n  count = 3\n  values(1)%str = 'a'\n  values(2)%str = 'b'\n  values(3)%str = 'c'\n  call set_array_variable_string_t(shell, values(1:count), count)\n  if (.not. allocated(shell%variables(1)%array_values)) error stop 1\n  if (size(shell%variables(1)%array_values) /= 3) error stop 2\n  if (trim(shell%variables(1)%array_values(1)%str) /= 'a') error stop 3\n  if (trim(shell%variables(1)%array_values(2)%str) /= 'b') error stop 4\n  if (trim(shell%variables(1)%array_values(3)%str) /= 'c') error stop 5\n  print *, trim(shell%variables(1)%array_values(1)%str), trim(shell%variables(1)%array_values(2)%str), trim(shell%variables(1)%array_values(3)%str)\ncontains\n  subroutine set_array_variable_string_t(shell, values, count)\n    type(shell_t), intent(inout) :: shell\n    type(string_t), intent(in) :: values(:)\n    integer, intent(in) :: count\n    integer :: k\n    allocate(shell%variables(1)%array_values(count))\n    do k = 1, count\n      shell%variables(1)%array_values(k)%str = values(k)%str\n    end do\n    shell%variables(1)%array_size = count\n  end subroutine\nend program\n",
        "f90",
    );
    let out = unique_path("derived_array_section_actual", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived array section actual compile failed to spawn");
    assert!(
        compile.status.success(),
        "derived array section actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("derived array section actual run failed");
    assert!(
        run.status.success(),
        "derived array section actual run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("a") && stdout.contains("b") && stdout.contains("c"),
        "derived array section actual should preserve section bounds and contents: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_derived_shell_initialization_survives_large_component_offsets() {
    let src = write_program(
        "program p\n  implicit none\n  type :: string_t\n    character(:), allocatable :: str\n  end type string_t\n  type :: shell_t\n    integer :: pad(50000) = 0\n    type(string_t), allocatable :: local_vars(:,:)\n    integer, allocatable :: local_var_counts(:)\n    type(string_t), allocatable :: positional_params(:)\n  end type shell_t\n  type(shell_t), allocatable :: shell\n  allocate(shell)\n  call initialize_shell(shell)\n  if (.not. allocated(shell%local_vars)) stop 10\n  if (.not. allocated(shell%local_var_counts)) stop 11\n  if (.not. allocated(shell%positional_params)) stop 12\n  if (shell%local_var_counts(1) /= 1) stop 13\n  print *, trim(shell%positional_params(1)%str)\ncontains\n  subroutine initialize_shell(shell)\n    type(shell_t), intent(out) :: shell\n    allocate(shell%local_vars(1, 1))\n    allocate(shell%local_var_counts(1))\n    allocate(shell%positional_params(1))\n    shell%local_var_counts = [1]\n    shell%positional_params(1)%str = 'ok'\n  end subroutine initialize_shell\nend program\n",
        "f90",
    );
    let out = unique_path("allocatable_derived_shell_bigpad", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable derived shell bigpad compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocatable derived shell bigpad compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("allocatable derived shell bigpad run failed");
    assert!(
        run.status.success(),
        "allocatable derived shell bigpad run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "allocatable derived shell bigpad should initialize nested components: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn c_f_pointer_array_target_builds_descriptor_backing() {
    let src = write_program(
        "program p\n  use iso_c_binding\n  implicit none\n  character(kind=c_char), target :: buf(4)\n  type(c_ptr) :: raw\n  character(kind=c_char), pointer :: view(:)\n  buf = [achar(111, kind=c_char), achar(107, kind=c_char), c_null_char, achar(120, kind=c_char)]\n  raw = c_loc(buf)\n  call c_f_pointer(raw, view, [4])\n  if (.not. associated(view)) stop 1\n  if (view(1) /= buf(1)) stop 2\n  if (view(2) /= buf(2)) stop 3\n  if (view(3) /= c_null_char) stop 4\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("c_f_pointer_array", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("c_f_pointer array compile failed to spawn");
    assert!(
        compile.status.success(),
        "c_f_pointer array compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("c_f_pointer array run failed");
    assert!(
        run.status.success(),
        "c_f_pointer array run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fixed_c_char_array_element_assignment_compiles_and_runs() {
    let src = write_program(
        "program p\n  use iso_c_binding\n  implicit none\n  character(kind=c_char), target :: buf(4)\n  integer :: i\n  do i = 1, 3\n    buf(i) = achar(96 + i, kind=c_char)\n  end do\n  buf(4) = c_null_char\n  if (buf(1) /= achar(97, kind=c_char)) stop 1\n  if (buf(2) /= achar(98, kind=c_char)) stop 2\n  if (buf(3) /= achar(99, kind=c_char)) stop 3\n  if (buf(4) /= c_null_char) stop 4\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("fixed_c_char_array_store", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("fixed c_char array compile failed to spawn");
    assert!(
        compile.status.success(),
        "fixed c_char array compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("fixed c_char array run failed");
    assert!(
        run.status.success(),
        "fixed c_char array run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fixed_c_char_array_null_scan_compiles_and_runs() {
    let src = write_program(
        "program p\n  use iso_c_binding\n  implicit none\n  character(kind=c_char), target :: buf(256)\n  integer :: i\n  buf = c_null_char\n  buf(1) = achar(97, kind=c_char)\n  do i = 1, 256\n    if (buf(i) == c_null_char) exit\n  end do\n  if (i /= 2) stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("fixed_c_char_array_null_scan", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("fixed c_char null scan compile failed to spawn");
    assert!(
        compile.status.success(),
        "fixed c_char null scan compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("fixed c_char null scan run failed");
    assert!(
        run.status.success(),
        "fixed c_char null scan run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_param_c_char_array_scan_in_char_result_function_runs() {
    let src = write_program(
        "module constants\n  implicit none\n  integer, parameter :: path_cap = 8\nend module constants\n\nmodule m\n  use iso_c_binding\n  use constants, only: path_cap\ncontains\n  function get_path() result(path)\n    character(len=:), allocatable :: path\n    character(kind=c_char), target :: c_path(path_cap)\n    integer :: i\n    c_path = c_null_char\n    c_path(1) = achar(97, kind=c_char)\n    c_path(2) = c_null_char\n    do i = 1, path_cap\n      if (c_path(i) == c_null_char) exit\n    end do\n    allocate(character(len=i-1) :: path)\n    do i = 1, len(path)\n      path(i:i) = c_path(i)\n    end do\n  end function\nend module m\n\nprogram p\n  use m, only: get_path\n  implicit none\n  if (get_path() /= 'a') error stop 1\n  print *, trim(get_path())\nend program\n",
        "f90",
    );
    let out = unique_path("imported_param_c_char_scan", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("imported-param c_char scan compile failed to spawn");
    assert!(
        compile.status.success(),
        "imported-param c_char scan should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("imported-param c_char scan run failed");
    assert!(
        run.status.success(),
        "imported-param c_char scan should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("a"),
        "unexpected imported-param c_char scan output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn c_loc_on_allocatable_c_char_array_element_compiles_and_runs() {
    let src = write_program(
        "program p\n  use iso_c_binding\n  implicit none\n  character(kind=c_char), allocatable, target :: c_tokens(:,:)\n  type(c_ptr) :: raw\n  integer :: i\n  allocate(c_tokens(4, 1))\n  do i = 1, 3\n    c_tokens(i, 1) = achar(96 + i, kind=c_char)\n  end do\n  c_tokens(4, 1) = c_null_char\n  raw = c_loc(c_tokens(1, 1))\n  if (.not. c_associated(raw)) stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("cloc_alloc_c_char_element", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("c_loc allocatable c_char compile failed to spawn");
    assert!(
        compile.status.success(),
        "c_loc allocatable c_char compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("c_loc allocatable c_char run failed");
    assert!(
        run.status.success(),
        "c_loc allocatable c_char run failed: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn named_len_char_component_substring_and_trim_compile() {
    let src = write_program(
        "module m\n  implicit none\n  integer, parameter :: max_token_len = 8\n  type :: token_t\n    character(len=max_token_len) :: value\n  end type token_t\ncontains\n  subroutine f(tok, i, is_bang, trimmed)\n    type(token_t), intent(in) :: tok\n    integer, intent(in) :: i\n    logical, intent(out) :: is_bang\n    character(len=max_token_len), intent(out) :: trimmed\n    is_bang = (tok%value(i:i) == '!')\n    trimmed = trim(tok%value)\n  end subroutine f\nend module m\n",
        "f90",
    );
    let out = unique_path("named_len_char_component", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("named-len char component compile failed to spawn");
    assert!(
        compile.status.success(),
        "named-len char component compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(
        out.exists(),
        "named-len char component should produce an object file"
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fixed_char_component_assigns_into_char_array_element() {
    let src = write_program(
        "program p\n  implicit none\n  integer, parameter :: max_token_len = 16\n  type :: token_t\n    character(len=max_token_len) :: value\n    logical :: quoted = .false.\n    logical :: escaped = .false.\n    integer :: quote_type = 0\n    integer :: value_length = 0\n  end type token_t\n  type(token_t) :: tok\n  character(len=max_token_len) :: words(1)\n  tok%value = 'echo'\n  words(1) = tok%value\n  if (trim(words(1)) /= 'echo') error stop 1\n  print *, trim(words(1))\nend program p\n",
        "f90",
    );
    let out = unique_path("fixed_char_component_array_store", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("fixed char component array store compile failed to spawn");
    assert!(
        compile.status.success(),
        "fixed char component array store compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("fixed char component array store run failed");
    assert!(
        run.status.success(),
        "fixed char component array store run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("echo"),
        "unexpected fixed char component array store output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fixed_char_array_component_element_assignment_round_trips() {
    let src = write_program(
        "module m\n  implicit none\n  type :: t\n    character(len=16) :: arr(4)\n  end type\n  type(t), save :: g\ncontains\n  subroutine fill_direct()\n    g%arr = ''\n    g%arr(1) = 'alpha'\n    g%arr(2) = 'beta'\n  end subroutine\n\n  subroutine fill_via_local_copy()\n    type(t) :: x\n    x%arr = ''\n    x%arr(1) = 'one'\n    x%arr(2) = 'two'\n    g = x\n  end subroutine\nend module\n\nprogram p\n  use m\n  call fill_direct()\n  if (trim(g%arr(1)) /= 'alpha') error stop 1\n  if (trim(g%arr(2)) /= 'beta') error stop 2\n  call fill_via_local_copy()\n  if (trim(g%arr(1)) /= 'one') error stop 3\n  if (trim(g%arr(2)) /= 'two') error stop 4\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("fixed_char_array_component_element", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("fixed char array component element compile failed to spawn");
    assert!(
        compile.status.success(),
        "fixed char array component element compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("fixed char array component element run failed");
    assert!(
        run.status.success(),
        "fixed char array component element run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected fixed char array component element output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fixed_char_array_component_element_actual_to_char_function_runs() {
    let src = write_program(
        "module m\n  implicit none\n  integer, parameter :: max_path_len = 256, max_dir_stack = 32\n  type :: dir_stack_t\n    character(len=max_path_len) :: directories(max_dir_stack)\n    integer :: top\n  end type\ncontains\n  function echo_path(path) result(out)\n    character(len=*), intent(in) :: path\n    character(len=max_path_len) :: out\n    out = path\n  end function\nend module\n\nprogram p\n  use m\n  implicit none\n  type(dir_stack_t) :: s\n  character(len=max_path_len) :: fixed\n  s%directories = ''\n  s%directories(2) = '/tmp'\n  fixed = echo_path(s%directories(2))\n  if (trim(fixed) /= '/tmp') error stop 1\n  print *, trim(fixed)\nend program\n",
        "f90",
    );
    let out = unique_path("fixed_char_component_actual_char_function", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("fixed char component actual char function compile failed to spawn");
    assert!(
        compile.status.success(),
        "fixed char component actual char function compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("fixed char component actual char function run failed");
    assert!(
        run.status.success(),
        "fixed char component actual char function run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("/tmp"),
        "unexpected fixed char component actual char function output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn deferred_char_component_array_element_assignment_round_trips() {
    let src = write_program(
        "program p\n  implicit none\n  type :: simple_command_data_t\n    character(len=:), allocatable :: words(:)\n    integer :: num_words = 0\n  end type simple_command_data_t\n  type :: command_node_t\n    type(simple_command_data_t) :: simple_cmd\n  end type command_node_t\n  type(command_node_t), pointer :: node\n  character(len=32) :: words(1)\n  words(1) = 'true'\n  node => create_simple_command(words, 1)\n  if (trim(node%simple_cmd%words(1)) /= 'true') error stop 1\n  print *, trim(node%simple_cmd%words(1))\ncontains\n  function create_simple_command(words, num_words) result(node)\n    character(len=*), intent(in) :: words(:)\n    integer, intent(in) :: num_words\n    type(command_node_t), pointer :: node\n    integer :: i\n    allocate(node)\n    allocate(character(len=32) :: node%simple_cmd%words(num_words))\n    node%simple_cmd%num_words = num_words\n    do i = 1, num_words\n      node%simple_cmd%words(i) = words(i)\n    end do\n  end function create_simple_command\nend program p\n",
        "f90",
    );
    let out = unique_path("deferred_char_component_array_assign", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("deferred char component array assign compile failed to spawn");
    assert!(
        compile.status.success(),
        "deferred char component array assign compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("deferred char component array assign run failed");
    assert!(
        run.status.success(),
        "deferred char component array assign run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("true"),
        "unexpected deferred char component array assign output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_fixed_char_actual_to_assumed_len_dummy_round_trips() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=32), allocatable :: words(:)\n  allocate(words(1))\n  words(1) = 'true'\n  call check(words)\ncontains\n  subroutine check(words)\n    character(len=*), intent(in) :: words(:)\n    if (trim(words(1)) /= 'true') error stop 1\n    print *, trim(words(1))\n  end subroutine check\nend program p\n",
        "f90",
    );
    let out = unique_path("alloc_fixed_char_actual_to_assumed_len", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable fixed-char actual compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocatable fixed-char actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("allocatable fixed-char actual run failed");
    assert!(
        run.status.success(),
        "allocatable fixed-char actual run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("true"),
        "unexpected allocatable fixed-char actual output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fixed_char_array_actual_to_assumed_len_dummy_reads_second_element() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=8) :: tokens(2)\n  tokens(1) = 'read'\n  tokens(2) = 'line'\n  call check(tokens)\ncontains\n  subroutine check(tokens)\n    character(len=*), intent(in) :: tokens(:)\n    if (trim(tokens(2)) /= 'line') error stop 1\n    print *, trim(tokens(2))\n  end subroutine check\nend program p\n",
        "f90",
    );
    let out = unique_path("fixed_char_actual_to_assumed_len_second", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("fixed char actual compile failed to spawn");
    assert!(
        compile.status.success(),
        "fixed char actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("fixed char actual run failed");
    assert!(
        run.status.success(),
        "fixed char actual run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("line"),
        "unexpected fixed char actual output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn whole_fixed_char_array_scalar_fill_preserves_element_slots() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=8) :: tokens(2)\n  tokens = ''\n  tokens(2) = 'line'\n  if (trim(tokens(2)) /= 'line') error stop 1\n  print *, trim(tokens(2))\nend program p\n",
        "f90",
    );
    let out = unique_path("whole_fixed_char_array_scalar_fill", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("whole fixed char array scalar fill compile failed to spawn");
    assert!(
        compile.status.success(),
        "whole fixed char array scalar fill compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("whole fixed char array scalar fill run failed");
    assert!(
        run.status.success(),
        "whole fixed char array scalar fill run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("line"),
        "unexpected whole fixed char array scalar fill output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn derived_array_element_fixed_char_component_survives_nested_dummy_call() {
    let src = write_program(
        "program p\n  implicit none\n\n  type :: command_t\n    integer :: num_tokens = 0\n    character(len=32), allocatable :: tokens(:)\n  end type command_t\n\n  type :: pipeline_t\n    type(command_t), allocatable :: commands(:)\n    integer :: num_commands = 0\n  end type pipeline_t\n\n  type(pipeline_t) :: pipeline\n\n  allocate(pipeline%commands(1))\n  pipeline%num_commands = 1\n  pipeline%commands(1)%num_tokens = 1\n  allocate(character(len=32) :: pipeline%commands(1)%tokens(1))\n  pipeline%commands(1)%tokens(1) = 'true'\n\n  call exec(pipeline)\n\ncontains\n\n  subroutine exec(p)\n    type(pipeline_t), intent(inout) :: p\n    call run_single(p%commands(1))\n  end subroutine exec\n\n  subroutine run_single(cmd)\n    type(command_t), intent(inout) :: cmd\n    if (trim(cmd%tokens(1)) /= 'true') error stop 1\n    print *, 'ok'\n  end subroutine run_single\n\nend program p\n",
        "f90",
    );
    let out = unique_path("derived_array_element_fixed_char_component", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived array element fixed char component compile failed to spawn");
    assert!(
        compile.status.success(),
        "derived array element fixed char component compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("derived array element fixed char component run failed");
    assert!(
        run.status.success(),
        "derived array element fixed char component run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected derived array element fixed char component output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn module_global_derived_array_fixed_char_component_clears_cleanly() {
    let src = write_program(
        "module cache_mod\n  implicit none\n  integer, parameter :: max_entries = 4\n  type :: entry_t\n    logical :: valid = .false.\n    character(len=256) :: command = ''\n  end type entry_t\n  type(entry_t) :: command_cache(max_entries)\ncontains\n  subroutine clear_command_cache()\n    integer :: i\n    do i = 1, max_entries\n      command_cache(i)%valid = .false.\n      command_cache(i)%command = ''\n    end do\n  end subroutine clear_command_cache\nend module cache_mod\n\nprogram p\n  use cache_mod, only: clear_command_cache, command_cache\n  implicit none\n  call clear_command_cache()\n  if (len_trim(command_cache(1)%command) /= 0) error stop 1\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("module_global_derived_array_fixed_char_component", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("module global derived array fixed char component compile failed to spawn");
    assert!(
        compile.status.success(),
        "module global derived array fixed char component compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("module global derived array fixed char component run failed");
    assert!(
        run.status.success(),
        "module global derived array fixed char component run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected module global derived array fixed char component output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn deferred_char_component_array_element_actual_to_assumed_len_dummy_survives() {
    let src = write_program(
        "program p\n  implicit none\n  type :: command_t\n    character(len=:), allocatable :: tokens(:)\n  end type command_t\n  type(command_t) :: cmd\n  allocate(character(len=32) :: cmd%tokens(1))\n  cmd%tokens(1) = 'true'\n  if (is_keyword(cmd%tokens(1))) error stop 1\n  print *, trim(cmd%tokens(1))\ncontains\n  function is_keyword(word) result(ok)\n    character(len=*), intent(in) :: word\n    logical :: ok\n    ok = trim(word) == 'if'\n  end function is_keyword\nend program p\n",
        "f90",
    );
    let out = unique_path("deferred_char_component_actual_to_assumed_len", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("deferred char component actual compile failed to spawn");
    assert!(
        compile.status.success(),
        "deferred char component actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("deferred char component actual run failed");
    assert!(
        run.status.success(),
        "deferred char component actual run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("true"),
        "unexpected deferred char component actual output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn select_case_on_trimmed_deferred_char_component_dispatches_correctly() {
    let src = write_program(
        "program p\n  implicit none\n  type :: command_t\n    character(len=:), allocatable :: tokens(:)\n  end type command_t\n  type(command_t) :: cmd\n  integer :: code\n  allocate(character(len=8) :: cmd%tokens(1))\n  cmd%tokens(1) = 'echo'\n  code = dispatch(cmd)\n  if (code /= 42) error stop 1\n  print *, 'ok'\ncontains\n  integer function dispatch(cmd) result(code)\n    type(command_t), intent(in) :: cmd\n    select case (trim(cmd%tokens(1)))\n    case ('echo')\n      code = 42\n    case default\n      code = 0\n    end select\n  end function dispatch\nend program p\n",
        "f90",
    );
    let out = unique_path("select_case_trimmed_deferred_char_component", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("select-case deferred char component compile failed to spawn");
    assert!(
        compile.status.success(),
        "select-case deferred char component compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("select-case deferred char component run failed");
    assert!(
        run.status.success(),
        "select-case deferred char component run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected select-case deferred char component output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn scalar_component_actual_to_intent_out_dummy_updates_field() {
    let src = write_program(
        "program p\n  implicit none\n  type :: state_t\n    integer :: num_tokens = 0\n  end type state_t\n  type(state_t) :: state\n  call set_num(state%num_tokens)\n  if (state%num_tokens /= 2) error stop 1\n  print *, state%num_tokens\ncontains\n  subroutine set_num(n)\n    integer, intent(out) :: n\n    n = 2\n  end subroutine set_num\nend program p\n",
        "f90",
    );
    let out = unique_path("component_intent_out", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("component intent(out) compile failed to spawn");
    assert!(
        compile.status.success(),
        "component intent(out) compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("component intent(out) run failed");
    assert!(
        run.status.success(),
        "component intent(out) run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains('2'),
        "unexpected component intent(out) output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_two_dimensional_element_actuals_update_storage() {
    let src = write_program(
        "program p\n  implicit none\n  integer, allocatable :: grid(:,:)\n  allocate(grid(2, 1))\n  grid = 0\n  call set_pair(grid(1, 1), grid(2, 1))\n  if (grid(1, 1) /= 11) error stop 1\n  if (grid(2, 1) /= 22) error stop 2\n  print *, grid(1, 1), grid(2, 1)\ncontains\n  subroutine set_pair(x, y)\n    integer, intent(out) :: x, y\n    x = 11\n    y = 22\n  end subroutine set_pair\nend program p\n",
        "f90",
    );
    let out = unique_path("alloc_2d_element_actuals", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable 2d element-actual compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocatable 2d element-actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("allocatable 2d element-actual run failed");
    assert!(
        run.status.success(),
        "allocatable 2d element-actual run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("11 22") || stdout.contains("11  22"),
        "unexpected allocatable 2d element-actual output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_derived_array_global_component_access_compiles() {
    let dir = unique_dir("derived_array_global");
    let dep = write_program_in(
        &dir,
        "dep.f90",
        "module dep\n  implicit none\n  type :: item_t\n    logical :: active = .false.\n  end type item_t\n  type(item_t), save :: items(2)\ncontains\n  subroutine init_items()\n    items(1)%active = .true.\n  end subroutine init_items\nend module dep\n",
    );
    let user = write_program_in(
        &dir,
        "user.f90",
        "module user_mod\n  use dep, only: items\n  implicit none\ncontains\n  logical function item_active(i)\n    integer, intent(in) :: i\n    item_active = items(i)%active\n  end function item_active\nend module user_mod\n",
    );
    let dep_obj = dir.join("dep.o");
    let user_obj = dir.join("user.o");

    let dep_compile = Command::new(compiler("armfortas"))
        .args([
            "-c",
            dep.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            "-o",
            dep_obj.to_str().unwrap(),
        ])
        .output()
        .expect("dep module compile failed to spawn");
    assert!(
        dep_compile.status.success(),
        "dep module compile failed: {}",
        String::from_utf8_lossy(&dep_compile.stderr)
    );

    let user_compile = Command::new(compiler("armfortas"))
        .args([
            "-c",
            user.to_str().unwrap(),
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            "-o",
            user_obj.to_str().unwrap(),
        ])
        .output()
        .expect("user module compile failed to spawn");
    assert!(
        user_compile.status.success(),
        "user module compile failed: {}",
        String::from_utf8_lossy(&user_compile.stderr)
    );

    let _ = std::fs::remove_file(&dep_obj);
    let _ = std::fs::remove_file(&user_obj);
    let _ = std::fs::remove_file(dir.join("dep.amod"));
    let _ = std::fs::remove_file(&dep);
    let _ = std::fs::remove_file(&user);
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn derived_array_element_assignment_with_pointer_component_compiles() {
    let src = write_program(
        "module m\n  implicit none\n  type :: node_t\n    integer :: x = 0\n  end type node_t\n  type :: entry_t\n    character(len=256) :: name\n    type(node_t), pointer :: body => null()\n  end type entry_t\n  type(entry_t), save :: entries(4)\ncontains\n  subroutine shift(i)\n    integer, intent(in) :: i\n    entries(i) = entries(i + 1)\n  end subroutine shift\nend module m\n",
        "f90",
    );
    let out = unique_path("derived_array_shift_ptr", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived array shift compile failed to spawn");
    assert!(
        compile.status.success(),
        "derived array shift compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn dash_o_equals_form_sets_output_path() {
    let src = write_program("program p\n  print *, 1\nend program\n", "f90");
    let out = unique_path("oeq", "o");
    let arg = format!("-o={}", out.display());
    let result = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), &arg])
        .output()
        .expect("compile failed to spawn");
    assert!(
        result.status.success(),
        "-o=path compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(out.exists(), "-o=path should produce the requested output");
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn duplicate_o_is_rejected() {
    let src = write_program("program p\n  print *, 1\nend program\n", "f90");
    let out_a = unique_path("dup_a", "bin");
    let out_b = unique_path("dup_b", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([
            src.to_str().unwrap(),
            "-o",
            out_a.to_str().unwrap(),
            "-o",
            out_b.to_str().unwrap(),
        ])
        .output()
        .expect("compile failed to spawn");
    assert!(!result.status.success(), "duplicate -o should fail");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("duplicate -o"),
        "expected duplicate -o diagnostic: {}",
        stderr
    );
    assert!(!out_a.exists(), "first output should not be produced");
    assert!(!out_b.exists(), "second output should not be produced");
    let _ = std::fs::remove_file(&src);
}

#[test]
fn multi_input_dash_c_produces_one_object_per_source() {
    let dir = unique_dir("multi_c_ok");
    write_program_in(&dir, "m.f90", "module m\n  integer :: x = 7\nend module\n");
    write_program_in(
        &dir,
        "user.f90",
        "program p\n  use m\n  print *, x\nend program\n",
    );
    let result = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args(["-c", "m.f90", "user.f90"])
        .output()
        .expect("compile failed to spawn");
    assert!(
        result.status.success(),
        "multi-input -c failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(dir.join("m.o").exists(), "module object was not written");
    assert!(dir.join("user.o").exists(), "user object was not written");
    assert!(
        dir.join("m.amod").exists(),
        "module interface was not written"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn multi_input_dash_c_with_o_is_rejected() {
    let dir = unique_dir("multi_c_err");
    write_program_in(&dir, "a.f90", "program a\n  print *, 1\nend program\n");
    write_program_in(&dir, "b.f90", "program b\n  print *, 2\nend program\n");
    let result = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args(["-c", "a.f90", "b.f90", "-o", "multi.o"])
        .output()
        .expect("compile failed to spawn");
    assert!(
        !result.status.success(),
        "multi-input -c with -o should fail"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("-o") && stderr.contains("multiple input files"),
        "expected -c/-o multi-input diagnostic: {}",
        stderr
    );
    assert!(
        !dir.join("multi.o").exists(),
        "no linked or object output should be produced"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn prebuilt_object_input_links_cleanly() {
    let src = write_program("program p\n  print *, 9\nend program\n", "f90");
    let obj = unique_path("link_only_obj", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", obj.to_str().unwrap()])
        .output()
        .expect("object compile failed to spawn");
    assert!(
        compile.status.success(),
        "object compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let exe = unique_path("link_only_obj", "bin");
    let link = Command::new(compiler("armfortas"))
        .args([obj.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("link-only spawn failed");
    assert!(
        link.status.success(),
        "prebuilt object link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    assert!(exe.exists(), "prebuilt object link should write the binary");

    let _ = std::fs::remove_file(&exe);
    let _ = std::fs::remove_file(&obj);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn prebuilt_archive_input_links_after_objects() {
    let dir = unique_dir("link_only_archive");
    let helper_src = write_program_in(
        &dir,
        "helper.f90",
        "subroutine helper()\n  print *, 7\nend subroutine helper\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  call helper()\nend program p\n",
    );

    let helper_obj = dir.join("helper.o");
    let compile_helper = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            helper_src.to_str().unwrap(),
            "-o",
            helper_obj.to_str().unwrap(),
        ])
        .output()
        .expect("helper compile spawn failed");
    assert!(
        compile_helper.status.success(),
        "helper compile failed: {}",
        String::from_utf8_lossy(&compile_helper.stderr)
    );

    let archive = dir.join("libhelper.a");
    let ar = Command::new("ar")
        .current_dir(&dir)
        .args([
            "rcs",
            archive.to_str().unwrap(),
            helper_obj.to_str().unwrap(),
        ])
        .output()
        .expect("archive spawn failed");
    assert!(
        ar.status.success(),
        "archive creation failed: {}",
        String::from_utf8_lossy(&ar.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("main compile spawn failed");
    assert!(
        compile_main.status.success(),
        "main compile failed: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("linked_archive");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            main_obj.to_str().unwrap(),
            archive.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("archive link spawn failed");
    assert!(
        link.status.success(),
        "prebuilt archive link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    assert!(
        exe.exists(),
        "prebuilt archive link should write the binary"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dash_capital_s_produces_assembly_text() {
    let src = write_program("program p\n  print *, 1\nend program\n", "f90");
    let out = unique_path("asm", "s");
    let result = Command::new(compiler("armfortas"))
        .args(["-S", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "-S compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let asm = std::fs::read_to_string(&out).expect("missing asm output");
    assert!(
        asm.contains("__TEXT"),
        ".s output should contain section directive"
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn dash_capital_e_preprocesses_only() {
    let src = write_program(
        "#define X 99\nprogram p\n  print *, X\nend program\n",
        "F90",
    );
    let out = unique_path("pp", "f90");
    let result = Command::new(compiler("armfortas"))
        .args(["-E", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "-E preprocess failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let pp = std::fs::read_to_string(&out).expect("missing preprocessed output");
    assert!(
        pp.contains(", 99"),
        "preprocessed text should expand the macro: {}",
        pp
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn dash_capital_e_without_o_writes_to_stdout() {
    let dir = unique_dir("pp_stdout");
    write_program_in(
        &dir,
        "hello.F90",
        "#define X 99\nprogram p\n  print *, X\nend program\n",
    );
    let result = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args(["-E", "hello.F90"])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "-E preprocess failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stdout = String::from_utf8_lossy(&result.stdout);
    assert!(
        stdout.contains(", 99"),
        "preprocessed output should be written to stdout: {}",
        stdout
    );
    assert!(
        !dir.join("hello").exists(),
        "default -E output should not create a bare-stem file"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn emit_ast_without_o_uses_ast_suffix() {
    let dir = unique_dir("emit_ast_default");
    write_program_in(
        &dir,
        "hello.f90",
        "program p\n  implicit none\n  print *, 1\nend program\n",
    );
    let result = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args(["--emit-ast", "hello.f90"])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "--emit-ast failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let ast_path = dir.join("hello.ast");
    assert!(
        ast_path.exists(),
        "default --emit-ast should create hello.ast"
    );
    let ast = std::fs::read_to_string(&ast_path).expect("missing AST output");
    assert!(
        ast.contains("Program"),
        "AST dump should contain Program node: {}",
        ast
    );
    assert!(
        !dir.join("hello").exists(),
        "default --emit-ast output should not create a bare-stem file"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn emit_tokens_without_o_uses_tokens_suffix() {
    let dir = unique_dir("emit_tokens_default");
    write_program_in(
        &dir,
        "hello.f90",
        "program p\n  implicit none\n  print *, 1\nend program\n",
    );
    let result = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args(["--emit-tokens", "hello.f90"])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "--emit-tokens failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let tokens_path = dir.join("hello.tokens");
    assert!(
        tokens_path.exists(),
        "default --emit-tokens should create hello.tokens"
    );
    let tokens = std::fs::read_to_string(&tokens_path).expect("missing token output");
    assert!(
        tokens.contains("Token { kind:") && tokens.contains("IntegerLiteral"),
        "token dump should contain token debug output: {}",
        tokens
    );
    assert!(
        !dir.join("hello").exists(),
        "default --emit-tokens output should not create a bare-stem file"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn do_loop_zero_step_is_rejected_before_codegen() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: i\n  do i = 1, 10, 0\n    print *, i\n  end do\nend program\n",
        "f90",
    );
    let out = unique_path("zero_step", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "zero-step DO loop should be rejected"
    );
    assert_eq!(
        result.status.code(),
        Some(1),
        "zero-step DO loop should stay a compile-time error"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("DO step must not be zero"),
        "expected zero-step loop diagnostic: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn overlapping_select_case_ranges_are_rejected_before_codegen() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: x\n  x = 7\n  select case (x)\n  case (1:10)\n    print *, 1\n  case (5:8)\n    print *, 2\n  end select\nend program\n",
        "f90",
    );
    let out = unique_path("select_case_overlap", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "overlapping SELECT CASE ranges should be rejected"
    );
    assert_eq!(
        result.status.code(),
        Some(1),
        "overlapping SELECT CASE ranges should stay a compile-time error"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("SELECT CASE selectors must be mutually exclusive"),
        "expected overlapping SELECT CASE diagnostic: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn multiple_select_case_defaults_are_rejected_before_codegen() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: x\n  x = 7\n  select case (x)\n  case default\n    print *, 0\n  case default\n    print *, 9\n  end select\nend program\n",
        "f90",
    );
    let out = unique_path("select_case_default", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "multiple CASE DEFAULT arms should be rejected"
    );
    assert_eq!(
        result.status.code(),
        Some(1),
        "multiple CASE DEFAULT arms should stay a compile-time error"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("SELECT CASE cannot contain multiple CASE DEFAULT arms"),
        "expected duplicate CASE DEFAULT diagnostic: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn logical_and_or_short_circuit_in_conditions() {
    let src = write_program(
        "program p\n  implicit none\n  if (.false. .and. boom()) stop 1\n  if (.true. .or. boom()) stop 2\ncontains\n  logical function boom()\n    error stop 7\n  end function boom\nend program\n",
        "f90",
    );
    let out = unique_path("short_circuit", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "short-circuit repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&out).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "short-circuit repro should not evaluate boom():\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn dash_capital_d_defines_preprocessor_macro() {
    let src = write_program(
        "#ifdef USE_C_STRINGS\n#define X 1\n#else\n#define X 0\n#endif\nprogram p\n  print *, X\nend program\n",
        "F90",
    );
    let out = unique_path("pp_define", "f90");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-DUSE_C_STRINGS",
            "-E",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "-D preprocess failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let pp = std::fs::read_to_string(&out).expect("missing preprocessed output");
    assert!(
        pp.contains(", 1"),
        "preprocessed text should take the defined branch: {}",
        pp
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn dash_capital_d_rejects_invalid_macro_name() {
    let src = write_program("program p\n  print *, 1\nend program\n", "f90");
    let result = Command::new(compiler("armfortas"))
        .args(["-D1BAD", src.to_str().unwrap(), "-c"])
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "-D with an invalid macro name should fail"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("invalid macro definition"),
        "expected invalid macro diagnostic, got: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
}

#[test]
fn std_f95_rejects_f2008_error_stop() {
    let src = write_program("program p\n  error stop 'oops'\nend program\n", "f90");
    let out = unique_path("f95", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([
            "--std=f95",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "--std=f95 should reject ERROR STOP"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("ERROR STOP") && stderr.contains("F2008"),
        "expected ERROR STOP / F2008 error: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
}

#[test]
fn std_space_form_is_accepted() {
    let src = write_program("program p\n  print *, 7\nend program\n", "f90");
    let out = unique_path("std_space", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([
            "--std",
            "f2018",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "--std f2018 should compile: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        out.exists(),
        "space-form --std should preserve the input path"
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn std_f77_rejects_free_form_source() {
    let src = write_program("program p\n  print *, 1\nend program\n", "f90");
    let out = unique_path("std_f77_free", "o");
    let result = Command::new(compiler("armfortas"))
        .args([
            "--std=f77",
            "-c",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "--std=f77 should reject free-form source"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("--std=f77 requires fixed-form source"),
        "expected fixed-form requirement: {}",
        stderr
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn std_f95_rejects_impure_prefix() {
    let src = write_program("impure subroutine s()\nend subroutine\n", "f90");
    let out = unique_path("std_impure", "o");
    let result = Command::new(compiler("armfortas"))
        .args([
            "--std=f95",
            "-c",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(!result.status.success(), "--std=f95 should reject IMPURE");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("IMPURE") && stderr.contains("F2008"),
        "expected IMPURE / F2008 error: {}",
        stderr
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn std_f95_rejects_abstract_type() {
    let src = write_program(
        "module m\n  type, abstract :: t\n  end type t\nend module m\n",
        "f90",
    );
    let out = unique_path("std_abstract", "o");
    let result = Command::new(compiler("armfortas"))
        .args([
            "--std=f95",
            "-c",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "--std=f95 should reject ABSTRACT type"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("ABSTRACT type") && stderr.contains("F2003"),
        "expected ABSTRACT type / F2003 error: {}",
        stderr
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn std_f77_rejects_module_in_fixed_form() {
    let src = write_program(
        "      module m\n      implicit none\n      end module m\n",
        "f",
    );
    let out = unique_path("std_f77_module", "o");
    let result = Command::new(compiler("armfortas"))
        .args([
            "--std=f77",
            "-c",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(!result.status.success(), "--std=f77 should reject MODULE");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("MODULE") && stderr.contains("F90"),
        "expected MODULE / F90 error: {}",
        stderr
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn help_and_version_use_last_flag_wins_precedence() {
    let help_then_version = Command::new(compiler("armfortas"))
        .args(["--help", "--version"])
        .output()
        .expect("spawn failed");
    assert!(help_then_version.status.success());
    let hv_stdout = String::from_utf8_lossy(&help_then_version.stdout);
    assert!(
        hv_stdout.trim_start().starts_with("armfortas "),
        "expected trailing --version to win: {}",
        hv_stdout
    );

    let version_then_help = Command::new(compiler("armfortas"))
        .args(["--version", "--help"])
        .output()
        .expect("spawn failed");
    assert!(version_then_help.status.success());
    let vh_stdout = String::from_utf8_lossy(&version_then_help.stdout);
    assert!(
        vh_stdout.contains("USAGE"),
        "expected trailing --help to win: {}",
        vh_stdout
    );
}

#[test]
fn response_file_supplies_arguments() {
    let src = write_program("program p\n  print *, 7\nend program\n", "f90");
    let out = unique_path("resp", "bin");
    let resp = unique_path("flags", "txt");
    std::fs::write(
        &resp,
        format!("-O1\n-o\n{}\n{}\n", out.display(), src.display()),
    )
    .unwrap();
    let result = Command::new(compiler("armfortas"))
        .arg(format!("@{}", resp.display()))
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "@response-file compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(out.exists(), "binary should exist after @file compile");
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&resp);
}

#[test]
fn diagnostics_format_json_is_rejected_until_implemented() {
    let src = write_program("program p\n  print *, 7\nend program\n", "f90");
    let out = unique_path("diag_json", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([
            "--diagnostics-format=json",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "--diagnostics-format=json should be rejected until implemented"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("JSON diagnostics are not yet implemented"),
        "expected explicit json-format diagnostic: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn nested_response_files_support_quotes_and_relative_paths() {
    let dir = unique_dir("rsp nested");
    let src = write_program_in(
        &dir,
        "file with spaces.f90",
        "program p\n  print *, 7\nend program\n",
    );
    let out = dir.join("binary with spaces");
    let inner = dir.join("inner.rsp");
    let outer = dir.join("outer.rsp");
    std::fs::write(
        &inner,
        format!("\"{}\"\n-o\n\"{}\"\n", src.display(), out.display()),
    )
    .unwrap();
    std::fs::write(&outer, "@inner.rsp\n").unwrap();

    let result = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .arg("@outer.rsp")
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "nested quoted response files should compile: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(out.exists(), "nested response file should produce output");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn accepted_but_unimplemented_flags_emit_warnings() {
    let src = write_program("program p\n  print *, 7\nend program\n", "f90");
    let out = unique_path("warn_flags", "o");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            "-g",
            "-fcheck=bounds",
            "-fmax-stack-var-size=64",
            "-frecursive",
            "-fbackslash",
            "-Wall",
            "-Wextra",
            "-Wpedantic",
            "-Wdeprecated",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "compile with accepted flags should still succeed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    for needle in [
        "-g is accepted, but debug info emission is not yet implemented",
        "-fcheck=bounds currently has no effect",
        "-fmax-stack-var-size is recognized but not yet implemented",
        "-frecursive is recognized but not yet implemented",
        "-fbackslash is recognized but string escape processing is not yet implemented",
        "-Wall is recognized but warning-group emission is not yet implemented",
        "-Wextra is recognized but warning-group emission is not yet implemented",
    ] {
        assert!(
            stderr.contains(needle),
            "missing warning `{}` in {}",
            needle,
            stderr
        );
    }
    assert!(
        !stderr
            .contains("-Wpedantic is recognized but warning-group emission is not yet implemented"),
        "pedantic should now be a real semantic warning group: {}",
        stderr
    );
    assert!(
        !stderr.contains(
            "-Wdeprecated is recognized but warning-group emission is not yet implemented"
        ),
        "deprecated should now be a real semantic warning group: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn fcheck_all_warns_about_partial_support() {
    let src = write_program("program p\n  print *, 7\nend program\n", "f90");
    let out = unique_path("warn_fcheck_all", "o");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            "-fcheck=all",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("-fcheck=all is accepted, but only array bounds checks exist today"),
        "expected -fcheck=all warning: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn werror_promotes_cli_warnings_to_errors() {
    let src = write_program("program p\n  print *, 7\nend program\n", "f90");
    let out = unique_path("werror_warn", "o");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            "-Wall",
            "-Werror",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "-Werror should promote CLI warnings"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains(
            "error: -Wall is recognized but warning-group emission is not yet implemented"
        ),
        "expected promoted CLI warning: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn unknown_warning_flag_emits_warning() {
    let src = write_program("program p\n  print *, 7\nend program\n", "f90");
    let out = unique_path("wunknown", "o");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            "-Weverything",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "unknown -W should warn but compile"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("unrecognized warning option '-Weverything'"),
        "expected unknown-warning diagnostic: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn wpedantic_warns_on_arithmetic_if() {
    let src = write_program(
        "program p\n  integer :: i\n  i = 0\n  if (i) 10, 20, 30\n10 continue\n20 continue\n30 continue\nend program\n",
        "f90",
    );
    let out = unique_path("wpedantic", "o");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            "-Wpedantic",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(result.status.success(), "pedantic compile failed");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("warning: arithmetic IF is an obsolescent feature"),
        "expected arithmetic IF warning: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn wdeprecated_warns_on_common_block() {
    let src = write_program(
        "program p\n  integer :: x\n  common /blk/ x\nend program\n",
        "f90",
    );
    let out = unique_path("wdeprecated", "o");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            "-Wdeprecated",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(result.status.success(), "deprecated compile failed");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("warning: COMMON block is an obsolescent feature"),
        "expected COMMON warning: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn unknown_warning_flag_can_be_suppressed() {
    let src = write_program("program p\n  print *, 7\nend program\n", "f90");
    let out = unique_path("wunknown_suppressed", "o");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            "-Weverything",
            "-Wno-unknown-warning-option",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "suppressed unknown -W should compile"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        !stderr.contains("unrecognized warning option"),
        "unknown-warning suppression should silence the warning: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn missing_response_file_uses_io_exit_code() {
    let result = Command::new(compiler("armfortas"))
        .arg("@/definitely/missing/armfortas_cli.rsp")
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "missing response file should fail"
    );
    assert_eq!(
        result.status.code(),
        Some(3),
        "response-file read failures should map to I/O exit code"
    );
}

#[test]
fn escaped_at_prefixed_input_is_treated_as_literal_filename() {
    let dir = unique_dir("at_input");
    write_program_in(&dir, "@file.f90", "program p\n  print *, 7\nend program\n");
    let out = dir.join("at_file.o");
    let result = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args(["-c", "@@file.f90", "-o", "at_file.o"])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "escaped @ input should compile: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        out.exists(),
        "escaped @ input should produce the object file"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dash_j_writes_amod_to_chosen_directory() {
    let src = write_program("module dashj_mod\n  integer :: y = 5\nend module\n", "f90");
    let out = unique_path("dashjobj", "o");
    let amod_dir = std::env::temp_dir().join(format!(
        "afs_cli_amod_{}_{}",
        std::process::id(),
        std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .unwrap()
            .as_nanos(),
    ));
    std::fs::create_dir_all(&amod_dir).unwrap();
    let result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            "-J",
            amod_dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "-J compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let amod = amod_dir.join("dashj_mod.amod");
    assert!(amod.exists(), "-J should place .amod in the requested dir");
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_dir_all(&amod_dir);
}

#[test]
fn dash_j_nonexistent_dir_is_hard_error() {
    let dir = unique_dir("dashj_bad");
    let src = write_program_in(
        &dir,
        "m.f90",
        "module dashj_mod\n  integer :: y = 5\nend module\n",
    );
    let out = dir.join("m.o");
    let missing = dir.join("missing_modules");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            "-J",
            missing.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(!result.status.success(), "-J to missing dir should fail");
    assert_eq!(result.status.code(), Some(3), "expected I/O exit code");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("cannot write"),
        "expected cannot-write diagnostic: {}",
        stderr
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dash_i_equals_form_finds_modules() {
    let dir = unique_dir("ieq_mod");
    let mod_src = write_program_in(
        &dir,
        "mymod.f90",
        "module mymod\n  integer :: x = 7\nend module\n",
    );
    let user_src = write_program_in(
        &dir,
        "use_mod.f90",
        "program p\n  use mymod\n  print *, x\nend program\n",
    );
    let mod_obj = dir.join("mymod.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .args([
            "-c",
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "module compile failed: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let user_obj = dir.join("use_mod.o");
    let include_arg = format!("-I={}", dir.display());
    let compile_user = Command::new(compiler("armfortas"))
        .args([
            &include_arg,
            "-c",
            user_src.to_str().unwrap(),
            "-o",
            user_obj.to_str().unwrap(),
        ])
        .output()
        .expect("user compile spawn failed");
    assert!(
        compile_user.status.success(),
        "-I=dir should find module interfaces: {}",
        String::from_utf8_lossy(&compile_user.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn block_use_imports_module_values_and_procedures() {
    let dir = unique_dir("block_use_mod");
    let mod_src = write_program_in(
        &dir,
        "expansion.f90",
        "module expansion\n  implicit none\n  integer, save :: base_value = 7\ncontains\n  function arithmetic_expansion_shell(expr, shell) result(r)\n    character(len=*), intent(in) :: expr\n    integer, intent(inout) :: shell\n    character(len=:), allocatable :: r\n    r = trim(expr)\n    shell = shell + 1\n  end function\nend module\n",
    );
    let user_src = write_program_in(
        &dir,
        "user.f90",
        "program p\n  implicit none\n  integer :: shell, total\n  character(len=32) :: var_value\n  integer :: actual_value_len\n  shell = 0\n  total = 0\n  var_value = '123'\n  actual_value_len = 3\n  block\n    use expansion, only: arithmetic_expansion_shell, base_value\n    character(len=:), allocatable :: arith_expr, arith_result\n    arith_expr = '$((' // var_value(:actual_value_len) // '))'\n    arith_result = arithmetic_expansion_shell(trim(arith_expr), shell)\n    total = base_value + len_trim(arith_result)\n  end block\n  if (shell /= 1) error stop 1\n  if (total /= 14) error stop 2\nend program\n",
    );
    let mod_obj = dir.join("expansion.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "module compile failed: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let user_obj = dir.join("user.o");
    let compile_user = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            user_src.to_str().unwrap(),
            "-o",
            user_obj.to_str().unwrap(),
        ])
        .output()
        .expect("user compile spawn failed");
    assert!(
        compile_user.status.success(),
        "BLOCK-local USE imports should compile: {}",
        String::from_utf8_lossy(&compile_user.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn block_interface_declares_callable_under_implicit_none() {
    let src = write_program(
        "subroutine s(acc_status)\n  use iso_c_binding, only: c_char, c_int\n  implicit none\n  integer, intent(out) :: acc_status\n  character(kind=c_char), target :: c_path(2)\n  block\n    interface\n      function cache_access(pathname, mode) bind(C, name=\"access\")\n        import :: c_char, c_int\n        character(kind=c_char), intent(in) :: pathname(*)\n        integer(c_int), value :: mode\n        integer(c_int) :: cache_access\n      end function\n    end interface\n    acc_status = cache_access(c_path, int(1, c_int))\n  end block\nend subroutine\n",
        "f90",
    );
    let out = unique_path("block_interface_decl", "o");
    let result = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "BLOCK-local interface procedures should count as declared: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn public_derived_type_in_private_module_is_emitted_and_importable() {
    let dir = unique_dir("public_type_mod");
    let mod_src = write_program_in(
        &dir,
        "m.f90",
        "module m\n  implicit none\n  private\n  public :: make_t\n  type, public :: result_t\n    integer :: source = 0\n    integer :: length = 0\n  end type\ncontains\n  function make_t() result(res)\n    type(result_t) :: res\n    res%source = 1\n    res%length = 2\n  end function\nend module\n",
    );
    let user_src = write_program_in(
        &dir,
        "user.f90",
        "program p\n  use m, only: make_t, result_t\n  implicit none\n  type(result_t) :: x\n  x = make_t()\n  if (x%source /= 1) error stop 1\n  if (x%length /= 2) error stop 2\nend program\n",
    );
    let mod_obj = dir.join("m.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "module compile failed: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let amod = dir.join("m.amod");
    let amod_text = std::fs::read_to_string(&amod).expect("module .amod should exist");
    assert!(
        amod_text.contains("@type result_t"),
        "public derived type should be exported to .amod: {}",
        amod_text
    );
    assert!(
        amod_text.contains("@field source") && amod_text.contains("@field length"),
        "derived type layout should be exported to .amod: {}",
        amod_text
    );

    let user_obj = dir.join("user.o");
    let compile_user = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            user_src.to_str().unwrap(),
            "-o",
            user_obj.to_str().unwrap(),
        ])
        .output()
        .expect("user compile spawn failed");
    assert!(
        compile_user.status.success(),
        "consumer compile should import the public derived type layout: {}",
        String::from_utf8_lossy(&compile_user.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_named_char_component_lengths_round_trip_through_amod_and_run() {
    let dir = unique_dir("named_char_component_amod");
    let cfg_src = write_program_in(
        &dir,
        "cfg.f90",
        "module cfg\n  implicit none\n  integer, parameter :: max_token_len = 16\nend module cfg\n",
    );
    let mod_src = write_program_in(
        &dir,
        "m.f90",
        "module m\n  use cfg, only: max_token_len\n  implicit none\n  type, public :: simple_command_t\n    character(len=max_token_len), allocatable :: assignments(:)\n    character(len=max_token_len) :: heredoc_delimiter = ''\n  end type\nend module m\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use m, only: simple_command_t\n  implicit none\n  type(simple_command_t) :: cmd\n  allocate(cmd%assignments(1))\n  cmd%assignments(1) = 'hello.world.txt'\n  cmd%heredoc_delimiter = 'done'\n  if (trim(cmd%assignments(1)) /= 'hello.world.txt') error stop 1\n  if (trim(cmd%heredoc_delimiter) /= 'done') error stop 2\n  print *, trim(cmd%assignments(1)), trim(cmd%heredoc_delimiter)\nend program\n",
    );

    let cfg_obj = dir.join("cfg.o");
    let compile_cfg = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            cfg_src.to_str().unwrap(),
            "-o",
            cfg_obj.to_str().unwrap(),
        ])
        .output()
        .expect("cfg compile spawn failed");
    assert!(
        compile_cfg.status.success(),
        "cfg module should compile: {}",
        String::from_utf8_lossy(&compile_cfg.stderr)
    );

    let mod_obj = dir.join("m.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "module with imported named character lengths should compile: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let amod = dir.join("m.amod");
    let amod_text = std::fs::read_to_string(&amod).expect("missing m.amod");
    assert!(
        amod_text.contains("@field assignments : character(len=16)")
            && amod_text.contains("@field heredoc_delimiter : character(len=16)"),
        "fixed imported character component lengths should survive into .amod: {}",
        amod_text
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("main compile spawn failed");
    assert!(
        compile_main.status.success(),
        "imported fixed-length character components should compile through .amod: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("named_char_component_amod.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            cfg_obj.to_str().unwrap(),
            mod_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("named char component link spawn failed");
    assert!(
        link.status.success(),
        "imported fixed-length character component objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("named char component run failed");
    assert!(
        run.status.success(),
        "imported fixed-length character components should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("hello.world.txt") && stdout.contains("done"),
        "imported fixed-length character components should preserve their bytes: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn nested_derived_defaults_initialize_locally() {
    let src = write_program(
        "program p\n  implicit none\n  type :: control_block_t\n    logical :: should_execute = .true.\n    character(len=4) :: marker = ''\n  end type control_block_t\n  type :: shell_state_t\n    integer :: control_depth = 0\n    type(control_block_t) :: control_stack(2)\n  end type shell_state_t\n  type(shell_state_t) :: shell\n  if (shell%control_depth /= 0) error stop 1\n  if (.not. shell%control_stack(1)%should_execute) error stop 2\n  if (shell%control_stack(2)%marker /= '    ') error stop 3\n  print *, shell%control_depth, shell%control_stack(1)%should_execute\nend program\n",
        "f90",
    );
    let out = unique_path("nested_derived_defaults_local", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("nested derived default-init compile failed to spawn");
    assert!(
        compile.status.success(),
        "nested derived defaults should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("nested derived default-init run failed");
    assert!(
        run.status.success(),
        "nested derived defaults should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains('0') && stdout.to_lowercase().contains('t'),
        "unexpected nested derived default-init output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_nested_derived_defaults_round_trip_through_amod_and_run() {
    let dir = unique_dir("nested_derived_defaults_amod");
    let mod_src = write_program_in(
        &dir,
        "state_mod.f90",
        "module state_mod\n  implicit none\n  type :: control_block_t\n    logical :: should_execute = .true.\n    character(len=4) :: marker = ''\n  end type control_block_t\n  type, public :: shell_state_t\n    integer :: control_depth = 0\n    type(control_block_t) :: control_stack(2)\n  end type shell_state_t\nend module state_mod\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use state_mod, only: shell_state_t\n  implicit none\n  type(shell_state_t) :: shell\n  if (shell%control_depth /= 0) error stop 1\n  if (.not. shell%control_stack(1)%should_execute) error stop 2\n  if (shell%control_stack(2)%marker /= '    ') error stop 3\n  print *, shell%control_depth, shell%control_stack(1)%should_execute\nend program\n",
    );

    let mod_obj = dir.join("state_mod.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("state module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "state module should compile: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let amod = dir.join("state_mod.amod");
    let amod_text = std::fs::read_to_string(&amod).expect("missing state_mod.amod");
    assert!(
        amod_text.contains("@init=int:0")
            && amod_text.contains("@init=logical:true")
            && amod_text.contains("@init=charhex:"),
        "nested derived field defaults should be exported to .amod: {}",
        amod_text
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("main compile spawn failed");
    assert!(
        compile_main.status.success(),
        "imported nested derived defaults should compile: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("nested_defaults.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            mod_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "nested derived default-init objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run spawn failed");
    assert!(
        run.status.success(),
        "imported nested derived defaults should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains('0') && stdout.to_lowercase().contains('t'),
        "unexpected imported nested derived default-init output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn derived_dummy_component_subscript_uses_updated_component_value() {
    let src = write_program(
        "program p\n  implicit none\n  type :: control_block_t\n    integer :: block_type = 0\n    logical :: should_execute = .true.\n  end type control_block_t\n  type :: shell_state_t\n    type(control_block_t) :: control_stack(20)\n    integer :: control_depth = 0\n  end type shell_state_t\n  type(shell_state_t) :: shell\n  call push(shell)\n  if (shell%control_depth /= 1) error stop 1\n  if (shell%control_stack(1)%block_type /= 7) error stop 2\n  if (.not. shell%control_stack(1)%should_execute) error stop 3\n  print *, shell%control_depth, shell%control_stack(1)%block_type\ncontains\n  subroutine push(shell)\n    type(shell_state_t), intent(inout) :: shell\n    shell%control_depth = shell%control_depth + 1\n    shell%control_stack(shell%control_depth)%block_type = 7\n  end subroutine push\nend program\n",
        "f90",
    );
    let out = unique_path("derived_dummy_component_subscript", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived dummy component-subscript compile failed to spawn");
    assert!(
        compile.status.success(),
        "derived dummy component-subscript should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("derived dummy component-subscript run failed");
    assert!(
        run.status.success(),
        "derived dummy component-subscript should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains('1') && stdout.contains('7'),
        "unexpected derived dummy component-subscript output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn shared_compile_emits_amod_and_links_cleanly() {
    let dir = unique_dir("shared_mod");
    let lib_src = write_program_in(
        &dir,
        "mylib.f90",
        "module m\ncontains\n  integer function answer()\n    answer = 42\n  end function\nend module\n",
    );
    let user_src = write_program_in(
        &dir,
        "user.f90",
        "program p\n  use m\n  print *, answer()\nend program\n",
    );
    let dylib = dir.join("libmylib.dylib");
    let shared = Command::new(compiler("armfortas"))
        .args([
            "-shared",
            lib_src.to_str().unwrap(),
            "-o",
            dylib.to_str().unwrap(),
        ])
        .output()
        .expect("shared compile spawn failed");
    assert!(
        shared.status.success(),
        "shared compile failed: {}",
        String::from_utf8_lossy(&shared.stderr)
    );
    assert!(
        dir.join("m.amod").exists(),
        "shared compile should emit m.amod"
    );

    let exe = dir.join("use_m");
    let dir_str = dir.to_str().unwrap();
    let user = Command::new(compiler("armfortas"))
        .args([
            "-I",
            dir_str,
            "-L",
            dir_str,
            "-rpath",
            dir_str,
            "-lmylib",
            user_src.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("user compile spawn failed");
    assert!(
        user.status.success(),
        "consumer link failed: {}",
        String::from_utf8_lossy(&user.stderr)
    );

    let run = Command::new(&exe).output().expect("consumer run failed");
    assert!(
        run.status.success(),
        "consumer run failed: {:?}",
        run.status
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.trim().ends_with("42"),
        "unexpected output: {}",
        stdout
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn verbose_flag_streams_phase_lines_to_stderr() {
    let src = write_program("program p\n  print *, 1\nend program\n", "f90");
    let out = unique_path("verbose", "bin");
    let result = Command::new(compiler("armfortas"))
        .args(["-v", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("preprocessing:"),
        "verbose missing preprocessing line: {}",
        stderr
    );
    assert!(
        stderr.contains("codegen:"),
        "verbose missing codegen line: {}",
        stderr
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn time_report_prints_phase_table() {
    let src = write_program("program p\n  print *, 1\nend program\n", "f90");
    let out = unique_path("timer", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([
            "--time-report",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("Phase"),
        "missing time-report header: {}",
        stderr
    );
    assert!(
        stderr.contains("Total"),
        "missing time-report total: {}",
        stderr
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn time_report_prints_phase_table_on_error() {
    let src = write_program("program p\n  error stop 'oops'\nend program\n", "f90");
    let out = unique_path("timer_err", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([
            "--time-report",
            "--std=f95",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "compile should fail under --std=f95"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("Phase"),
        "missing time-report header: {}",
        stderr
    );
    assert!(
        stderr.contains("Total"),
        "missing time-report total: {}",
        stderr
    );
    assert!(
        stderr.contains("sema"),
        "expected failing phase in report: {}",
        stderr
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn diagnostic_renders_source_line_and_caret() {
    let src = write_program("program p\n  error stop 'oops'\nend program\n", "f90");
    let out = unique_path("diag", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([
            "--std=f95",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(!result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    // Header line uses the gfortran/clang gutter format.
    assert!(
        stderr.contains(":2:3: error:"),
        "missing standard error header: {}",
        stderr
    );
    // Source line is shown with a numbered gutter (`    2 |`).
    assert!(
        stderr.contains("|   error stop"),
        "missing source-line snippet: {}",
        stderr
    );
    // Caret underline lives on a `      |` line.
    assert!(
        stderr.contains("      |"),
        "missing caret gutter: {}",
        stderr
    );
    assert!(stderr.contains("^"), "missing caret marker: {}", stderr);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn garbage_text_is_rejected_as_parse_error() {
    let src = write_program("this is garbage\n", "f90");
    let out = unique_path("garbage", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "garbage text should fail to parse"
    );
    assert_eq!(
        result.status.code(),
        Some(1),
        "garbage text should be a compile-time parse error"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("parse error:"),
        "expected parse-error header: {}",
        stderr
    );
    assert!(
        stderr.contains("| this is garbage"),
        "expected source snippet for parse error: {}",
        stderr
    );
    assert!(
        stderr.contains("^"),
        "expected parse-error caret: {}",
        stderr
    );
    assert!(
        !stderr.contains("linker failed"),
        "garbage text should not reach the linker: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn utf8_lexer_error_reports_character_and_caret() {
    let src = write_program("program p\n  café = 1\nend program\n", "f90");
    let out = unique_path("utf8", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(!result.status.success(), "UTF-8 lexer error should fail");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("lexer error: unexpected character: 'é'"),
        "expected full UTF-8 character in lexer diagnostic: {}",
        stderr
    );
    assert!(
        stderr.contains("|   café = 1"),
        "expected lexer source snippet: {}",
        stderr
    );
    assert!(stderr.contains("^"), "expected lexer caret: {}", stderr);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn bom_prefixed_source_compiles_cleanly() {
    let src = write_program("\u{feff}program p\n  print *, 1\nend program\n", "f90");
    let out = unique_path("bom", "o");
    let result = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "BOM-prefixed source should compile: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        out.exists(),
        "BOM-prefixed compile should produce an object"
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn deeply_nested_expression_fails_gracefully() {
    let expr = format!("{}1{}", "(".repeat(1500), ")".repeat(1500));
    let src = write_program(
        &format!("program p\n  integer :: x\n  x = {expr}\nend program\n"),
        "f90",
    );
    let out = unique_path("deep_expr", "o");
    let result = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "deeply nested expression should be rejected"
    );
    assert_eq!(
        result.status.code(),
        Some(1),
        "deep-expression overflow should stay a compile error"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("expression nesting exceeds parser limit"),
        "expected parser depth diagnostic: {}",
        stderr
    );
    assert!(
        !stderr.contains("INTERNAL COMPILER ERROR"),
        "depth guard should avoid ICE path: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn diagnostic_gutter_stays_aligned_for_six_digit_line_numbers() {
    let mut src_text = "! filler\n".repeat(100_000);
    src_text.push_str("program p\n  error stop 'oops'\nend program\n");
    let src = write_program(&src_text, "f90");
    let out = unique_path("bigline", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([
            "--std=f95",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "compile should fail under --std=f95"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    let lines: Vec<_> = stderr.lines().collect();
    let idx = lines
        .iter()
        .position(|line| line.contains("100002 |"))
        .expect("missing six-digit source gutter");
    let source_line = lines[idx];
    let caret_line = *lines.get(idx + 1).expect("missing caret line");
    assert_eq!(
        source_line.find('|'),
        caret_line.find('|'),
        "source and caret gutters should stay aligned:\n{}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn integer_pow_overflow_is_diagnosed() {
    let src = write_program("program p\n  print *, 2**200\nend program\n", "f90");
    let out = unique_path("pow_overflow", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "constant integer overflow should be rejected"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("compile-time integer overflow"),
        "expected integer overflow diagnostic: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn parameter_integer_literal_overflow_is_diagnosed() {
    let src = write_program(
        "program p\n  integer, parameter :: x = -2147483649\n  print *, x\nend program\n",
        "f90",
    );
    let out = unique_path("param_overflow", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "parameter overflow should be rejected"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("compile-time integer overflow"),
        "expected parameter overflow diagnostic: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn symbolic_integer_kind_suffix_uses_imported_width() {
    let src = write_program(
        "program p\n  use iso_c_binding, only: c_long\n  integer(c_long), parameter :: x = 9223372036854775807_c_long\n  if (x /= 9223372036854775807_c_long) error stop 1\n  print *, x\nend program\n",
        "f90",
    );
    let out = unique_path("symbolic_int_kind_ok", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "symbolic integer kind suffix should honor imported width: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "symbolic integer kind program should run: {}",
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn integer_division_by_zero_is_diagnosed() {
    let src = write_program(
        "program p\n  integer, parameter :: x = 1 / 0\n  print *, x\nend program\n",
        "f90",
    );
    let out = unique_path("div_zero", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "compile-time integer division by zero should be rejected"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("compile-time integer division by zero"),
        "expected division-by-zero diagnostic: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn no_color_env_suppresses_ansi_escapes() {
    let src = write_program("program p\n  error stop 'x'\nend program\n", "f90");
    let out = unique_path("nocolor", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([
            "--std=f95",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .env("NO_COLOR", "1")
        .env_remove("CLICOLOR_FORCE")
        .output()
        .expect("spawn failed");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        !stderr.contains('\x1b'),
        "NO_COLOR must suppress ANSI escapes: {:?}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn clicolor_force_enables_ansi_even_off_a_tty() {
    let src = write_program("program p\n  error stop 'x'\nend program\n", "f90");
    let out = unique_path("forcecolor", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([
            "--std=f95",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .env("CLICOLOR_FORCE", "1")
        .env_remove("NO_COLOR")
        .output()
        .expect("spawn failed");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains('\x1b'),
        "CLICOLOR_FORCE must produce ANSI escapes: {:?}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn fimplicit_none_rejects_implicitly_typed_use() {
    let src = write_program("program p\n  i = 5\n  print *, i\nend program\n", "f90");
    let out = unique_path("fimplicit", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-fimplicit-none",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "-fimplicit-none should reject undeclared 'i'"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("'i'") && stderr.contains("IMPLICIT NONE is active"),
        "expected implicit-none diagnostic: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fimplicit_none_respects_explicit_implicit_rules() {
    let src = write_program(
        "program p\n  implicit integer (i-n)\n  i = 5\n  print *, i\nend program\n",
        "f90",
    );
    let out = unique_path("fimplicit_explicit", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-fimplicit-none",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "explicit IMPLICIT should win over -fimplicit-none: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let run = Command::new(&out).output().expect("run failed");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.trim().ends_with('5'),
        "expected explicit implicit typing to remain active: {}",
        stdout
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fdefault_integer_8_changes_default_kind() {
    let src = write_program(
        "program p\n  integer :: x\n  print *, kind(x)\nend program\n",
        "f90",
    );
    let out = unique_path("defint", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-fdefault-integer-8",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "-fdefault-integer-8 compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let run = Command::new(&out).output().expect("run failed");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.trim().ends_with('8'),
        "expected kind 8: {:?}",
        stdout
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fdefault_real_8_changes_default_kind() {
    let src = write_program(
        "program p\n  real :: y\n  print *, kind(y)\nend program\n",
        "f90",
    );
    let out = unique_path("defreal", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-fdefault-real-8",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(result.status.success());
    let run = Command::new(&out).output().expect("run failed");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.trim().ends_with('8'),
        "expected kind 8: {:?}",
        stdout
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn afs_runtime_path_env_overrides_runtime_discovery() {
    // Point $AFS_RUNTIME_PATH at a directory that DOES contain the
    // real runtime and verify compilation still succeeds — exercises
    // the override branch end-to-end without hiding the real runtime.
    let rt = PathBuf::from("target/release/libarmfortas_rt.a");
    if !rt.exists() {
        // Skip silently when running off a tree that only has a
        // debug runtime — CI has both; a contributor's fresh clone
        // with only `cargo build` will hit release.
        return;
    }
    let rt_dir = rt.parent().unwrap().to_path_buf();
    let src = write_program("program p\n  print *, 11\nend program\n", "f90");
    let out = unique_path("rtpath", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("AFS_RUNTIME_PATH", &rt_dir)
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "AFS_RUNTIME_PATH-directed compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn missing_input_file_reports_io_error() {
    let result = Command::new(compiler("armfortas"))
        .args(["/nonexistent/path/source.f90"])
        .output()
        .expect("spawn failed");
    assert!(!result.status.success(), "missing input should fail");
    // Per sprint 32 #6 exit-code spec: I/O errors (cannot read input)
    // map to exit code 3.  The driver categorises by error message
    // text today; a structured error type is sprint 32 #507.
    assert_eq!(
        result.status.code(),
        Some(3),
        "missing input should map to exit code 3 (I/O error), got: {:?}",
        result.status
    );
}

#[test]
fn entry_statement_reports_not_implemented() {
    let src = write_program(
        "subroutine f(x)\n  integer :: x\n  entry g(y)\nend subroutine\n",
        "f90",
    );
    let out = unique_path("entry_stmt", "o");
    let result = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(!result.status.success(), "ENTRY should not compile yet");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("ENTRY statements are recognized but not yet implemented"),
        "expected explicit ENTRY diagnostic: {}",
        stderr
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn coarray_declaration_reports_not_implemented() {
    let src = write_program("program p\n  integer :: x[*]\nend program\n", "f90");
    let out = unique_path("coarray_decl", "o");
    let result = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "coarray declarations should fail honestly"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("coarray declarations are recognized but not yet implemented"),
        "expected explicit coarray declaration diagnostic: {}",
        stderr
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn coarray_sync_reports_not_implemented() {
    let src = write_program("program p\n  sync all\nend program\n", "f90");
    let out = unique_path("coarray_sync", "o");
    let result = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        !result.status.success(),
        "coarray SYNC should fail honestly"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("coarray SYNC statements are recognized but not yet implemented"),
        "expected explicit coarray SYNC diagnostic: {}",
        stderr
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn procedure_pointer_decl_compiles_through_wrapper_calls() {
    let src = write_program(
        "module m\n  implicit none\n  abstract interface\n    logical function pred(x)\n      integer, intent(in) :: x\n    end function pred\n    subroutine act(x)\n      integer, intent(in) :: x\n    end subroutine act\n  end interface\n  procedure(pred), pointer :: p => null()\n  procedure(act), pointer :: q => null()\ncontains\n  logical function ok(x)\n    integer, intent(in) :: x\n    ok = .false.\n    if (associated(p)) ok = p(x)\n  end function ok\n\n  subroutine run(x)\n    integer, intent(in) :: x\n    if (associated(q)) call q(x)\n  end subroutine run\nend module\n",
        "f90",
    );
    let out = unique_path("procedure_ptr_decl", "o");
    let result = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "procedure-pointer declarations should compile: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn procedure_pointer_calls_and_assignment_run_indirectly() {
    let src = write_program(
        "module m\n  implicit none\n  abstract interface\n    integer function pred(x)\n      integer, intent(in) :: x\n    end function pred\n    subroutine act(x)\n      integer, intent(inout) :: x\n    end subroutine act\n  end interface\n  procedure(pred), pointer :: p => null()\n  procedure(act), pointer :: q => null()\ncontains\n  integer function twice(x)\n    integer, intent(in) :: x\n    twice = x * 2\n  end function twice\n\n  subroutine bump(x)\n    integer, intent(inout) :: x\n    x = x + 1\n  end subroutine bump\n\n  subroutine init()\n    p => twice\n    q => bump\n  end subroutine init\nend module\n\nprogram main\n  use m\n  implicit none\n  integer :: x\n  call init()\n  x = p(3)\n  call q(x)\n  print *, x\nend program main\n",
        "f90",
    );
    let out = unique_path("procedure_ptr_run", "s");
    let compile = Command::new(compiler("armfortas"))
        .args(["-S", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "procedure-pointer indirect call program should lower to assembly: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let asm = std::fs::read_to_string(&out).expect("cannot read indirect-call assembly");
    assert!(
        asm.contains("blr "),
        "procedure-pointer calls should lower to BLR: {}",
        asm
    );
    assert!(
        asm.contains("_twice@PAGE") && asm.contains("_bump@PAGE"),
        "procedure-pointer assignment should materialize callee addresses: {}",
        asm
    );
    assert!(
        !asm.contains("bl _p") && !asm.contains("bl _q"),
        "procedure-pointer calls should not lower as direct symbol calls: {}",
        asm
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn procedure_dummy_actual_argument_round_trips_through_pointer_assignment() {
    let src = write_program(
        "module modproc_m\n  implicit none\n  abstract interface\n    subroutine cb(command, exit_status)\n      character(len=*), intent(in) :: command\n      integer, intent(out) :: exit_status\n    end subroutine cb\n  end interface\n  procedure(cb), pointer :: p => null()\ncontains\n  subroutine set_cb(x)\n    procedure(cb) :: x\n    p => x\n  end subroutine\n\n  subroutine run(command, exit_status)\n    character(len=*), intent(in) :: command\n    integer, intent(out) :: exit_status\n    call p(command, exit_status)\n  end subroutine\n\n  subroutine cb_impl(command, exit_status)\n    character(len=*), intent(in) :: command\n    integer, intent(out) :: exit_status\n    exit_status = len_trim(command)\n  end subroutine\nend module\n\nprogram main\n  use modproc_m\n  implicit none\n  integer :: status\n  call set_cb(cb_impl)\n  call run('abc', status)\n  print *, status\nend program\n",
        "f90",
    );
    let out = unique_path("procedure_dummy_actual", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "procedure dummy actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "procedure dummy actual runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains('3'),
        "procedure dummy actual should call the rebound module procedure: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn procedure_pointer_callback_with_derived_and_assumed_len_char_runs() {
    let src = write_program(
        "module m\n  implicit none\n  type :: shell_state_t\n    integer :: marker = 0\n  end type\n  type :: parser_state_t\n    character(len=:), allocatable :: raw_input\n  end type\n  abstract interface\n    subroutine cb(shell, command, out_len)\n      import :: shell_state_t\n      type(shell_state_t), intent(inout) :: shell\n      character(len=*), intent(in) :: command\n      integer, intent(out) :: out_len\n    end subroutine cb\n  end interface\n  procedure(cb), pointer :: p => null()\ncontains\n  subroutine set_cb(x)\n    procedure(cb) :: x\n    p => x\n  end subroutine\n\n  subroutine invoke(shell, command, out_len)\n    type(shell_state_t), intent(inout) :: shell\n    character(len=*), intent(in) :: command\n    integer, intent(out) :: out_len\n    call p(shell, command, out_len)\n  end subroutine\n\n  subroutine impl(shell, command, out_len)\n    type(shell_state_t), intent(inout) :: shell\n    character(len=*), intent(in) :: command\n    integer, intent(out) :: out_len\n    type(parser_state_t) :: state\n    state%raw_input = command\n    shell%marker = len(state%raw_input)\n    out_len = len(state%raw_input)\n    print '(A)', state%raw_input\n  end subroutine\nend module\n\nprogram main\n  use m\n  implicit none\n  type(shell_state_t) :: shell\n  integer :: n\n  call set_cb(impl)\n  call invoke(shell, 'echo a b c', n)\n  print *, shell%marker, n\nend program\n",
        "f90",
    );
    let out = unique_path("procptr_shell_char_component", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "procedure-pointer callback compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "procedure-pointer callback runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("echo a b c"),
        "procedure-pointer callback should preserve the assumed-length character payload: {}",
        stdout
    );
    assert!(
        stdout.contains("10"),
        "procedure-pointer callback should preserve the hidden character length: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn c_funloc_bind_c_handler_uses_binding_label_symbol() {
    let src = write_program(
        "module m\n  use iso_c_binding\n  implicit none\n  interface\n    function c_signal(sig, handler) bind(C, name='signal') result(old)\n      import :: c_int, c_funptr\n      integer(c_int), value :: sig\n      type(c_funptr), value :: handler\n      type(c_funptr) :: old\n    end function\n  end interface\ncontains\n  subroutine setup()\n    type(c_funptr) :: old_handler\n    old_handler = c_signal(2, c_funloc(sig_handler))\n  end subroutine\n\n  subroutine sig_handler() bind(C)\n  end subroutine\nend module\n",
        "f90",
    );
    let out = unique_path("c_funloc_bindc", "s");
    let compile = Command::new(compiler("armfortas"))
        .args(["-S", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "c_funloc bind(C) compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let asm = std::fs::read_to_string(&out).expect("cannot read c_funloc assembly");
    assert!(
        asm.contains("_sig_handler@PAGE"),
        "c_funloc should materialize the bind(C) label, not the module symbol: {}",
        asm
    );
    assert!(
        !asm.contains("_afs_modproc_m_sig_handler@PAGE"),
        "c_funloc should not reference the non-bind(C) module procedure symbol: {}",
        asm
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn c_funptr_component_assignment_round_trips_through_c_associated() {
    let src = write_program(
        "program p\n  use iso_c_binding\n  implicit none\n  type :: sigaction_t\n    type(c_funptr) :: sa_handler\n    integer(c_long) :: sa_mask(16)\n    integer(c_int) :: sa_flags\n    type(c_funptr) :: sa_restorer\n  end type\n  type(sigaction_t) :: sa\n  logical :: same\n  sa%sa_handler = c_funloc(handler)\n  same = c_associated(sa%sa_handler, c_funloc(handler))\n  print '(A,L1)', 'SAME=', same\ncontains\n  subroutine handler(signum) bind(C)\n    integer(c_int), value :: signum\n  end subroutine\nend program\n",
        "f90",
    );
    let out = unique_path("c_funptr_component_roundtrip", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("c_funptr component compile failed to spawn");
    assert!(
        compile.status.success(),
        "c_funptr component compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("c_funptr component run failed");
    assert!(
        run.status.success(),
        "c_funptr component runtime failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    let normalized: String = stdout.split_whitespace().collect();
    assert!(
        normalized.contains("SAME=T"),
        "c_funptr component should preserve the stored function pointer: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn c_ptr_array_element_assignment_round_trips_through_c_associated() {
    let src = write_program(
        "program p\n  use iso_c_binding\n  implicit none\n  character(len=16), target, allocatable :: args(:)\n  type(c_ptr), allocatable, target :: argv(:)\n  allocate(args(2))\n  allocate(argv(3))\n  args(1) = 'echo' // c_null_char\n  args(2) = 'done' // c_null_char\n  argv(1) = c_loc(args(1))\n  argv(2) = c_loc(args(2))\n  argv(3) = c_null_ptr\n  if (.not. c_associated(argv(1), c_loc(args(1)))) error stop 1\n  if (.not. c_associated(argv(2), c_loc(args(2)))) error stop 2\n  if (c_associated(argv(3))) error stop 3\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("c_ptr_array_element_roundtrip", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("c_ptr array element compile failed to spawn");
    assert!(
        compile.status.success(),
        "c_ptr array element compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("c_ptr array element run failed");
    assert!(
        run.status.success(),
        "c_ptr array element runtime failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected c_ptr array element output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn sigaction_module_bind_c_struct_preserves_handler_component_for_self_signal() {
    let src = write_program(
        "module m\n  use iso_c_binding\n  implicit none\n  integer(c_int), parameter :: SIGUSR1 = 10\n  logical, save :: pending(32) = .false.\n  type, bind(C) :: sigaction_t\n    type(c_funptr) :: sa_handler\n    integer(c_long) :: sa_mask(16)\n    integer(c_int) :: sa_flags\n    type(c_funptr) :: sa_restorer\n  end type\n  interface\n    function c_sigaction(signum, act, oldact) bind(C, name='sigaction')\n      import :: c_int, sigaction_t\n      integer(c_int), value :: signum\n      type(sigaction_t), intent(in) :: act\n      type(sigaction_t), intent(out) :: oldact\n      integer(c_int) :: c_sigaction\n    end function\n    function c_raise(sig) bind(C, name='raise')\n      import :: c_int\n      integer(c_int), value :: sig\n      integer(c_int) :: c_raise\n    end function\n  end interface\ncontains\n  subroutine handler(signum) bind(C)\n    integer(c_int), value :: signum\n    if (signum > 0 .and. signum <= 32) pending(signum) = .true.\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(sigaction_t) :: sa, old_sa\n  integer(c_int) :: rc\n  sa%sa_handler = c_funloc(handler)\n  sa%sa_mask = 0\n  sa%sa_flags = 0\n  sa%sa_restorer = c_null_funptr\n  rc = c_sigaction(SIGUSR1, sa, old_sa)\n  print '(A,I0)', 'SIGACTION=', rc\n  rc = c_raise(SIGUSR1)\n  print '(A,I0)', 'RAISE=', rc\n  print '(A,L1)', 'PENDING=', pending(SIGUSR1)\nend program\n",
        "f90",
    );
    let out = unique_path("sigaction_self_signal", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("sigaction self-signal compile failed to spawn");
    assert!(
        compile.status.success(),
        "sigaction self-signal compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("sigaction self-signal run failed");
    assert!(
        run.status.success(),
        "sigaction self-signal runtime failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    let normalized: String = stdout.split_whitespace().collect();
    assert!(
        normalized.contains("SIGACTION=0"),
        "sigaction setup should succeed: {}",
        stdout
    );
    assert!(
        normalized.contains("RAISE=0"),
        "self-signal should return normally through the registered handler: {}",
        stdout
    );
    assert!(
        normalized.contains("PENDING=T"),
        "signal handler should mark the pending flag through the BIND(C) struct: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn procedure_pointer_module_export_survives_amod_import() {
    let dir = unique_dir("procptr_amod");
    let mod_src = write_program_in(
        &dir,
        "control_flow.f90",
        "module control_flow\n  implicit none\n  abstract interface\n    subroutine evaluate_condition_interface(n)\n      integer, intent(inout) :: n\n    end subroutine\n  end interface\n  procedure(evaluate_condition_interface), pointer, public :: evaluate_condition => null()\nend module\n",
    );
    let user_src = write_program_in(
        &dir,
        "executor.f90",
        "module executor\n  implicit none\ncontains\n  subroutine init_control_flow_callbacks()\n    use control_flow\n    evaluate_condition => evaluate_condition_impl\n  end subroutine\n\n  subroutine evaluate_condition_impl(n)\n    integer, intent(inout) :: n\n    n = n + 1\n  end subroutine\nend module\n",
    );

    let mod_obj = dir.join("control_flow.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "procedure-pointer module should compile: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let user_obj = dir.join("executor.o");
    let compile_user = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            user_src.to_str().unwrap(),
            "-o",
            user_obj.to_str().unwrap(),
        ])
        .output()
        .expect("user compile spawn failed");
    assert!(
        compile_user.status.success(),
        "imported module procedure pointers should survive .amod export/import: {}",
        String::from_utf8_lossy(&compile_user.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn use_renamed_procedure_call_keeps_imported_target_even_with_local_name_collision() {
    let dir = unique_dir("use_rename_call_target");
    let imported_src = write_program_in(
        &dir,
        "imported.f90",
        "module imported_m\ncontains\n  subroutine builtin_type()\n    print *, 'IMPORTED'\n  end subroutine\nend module\n",
    );
    let wrapper_src = write_program_in(
        &dir,
        "wrapper.f90",
        "module wrapper_m\n  use imported_m, only: cmd_builtin_type => builtin_type\n  implicit none\ncontains\n  subroutine dispatch()\n    call cmd_builtin_type()\n  end subroutine\n\n  subroutine builtin_type()\n    print *, 'LOCAL'\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use wrapper_m\n  implicit none\n  call dispatch()\nend program\n",
    );

    let imported_obj = dir.join("imported.o");
    let compile_imported = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            imported_src.to_str().unwrap(),
            "-o",
            imported_obj.to_str().unwrap(),
        ])
        .output()
        .expect("imported module compile spawn failed");
    assert!(
        compile_imported.status.success(),
        "imported module should compile: {}",
        String::from_utf8_lossy(&compile_imported.stderr)
    );

    let wrapper_obj = dir.join("wrapper.o");
    let compile_wrapper = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            wrapper_src.to_str().unwrap(),
            "-o",
            wrapper_obj.to_str().unwrap(),
        ])
        .output()
        .expect("wrapper module compile spawn failed");
    assert!(
        compile_wrapper.status.success(),
        "wrapper module should preserve the USE-renamed import: {}",
        String::from_utf8_lossy(&compile_wrapper.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("main compile spawn failed");
    assert!(
        compile_main.status.success(),
        "main should compile against the wrapper module: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("use_rename_call_target.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            main_obj.to_str().unwrap(),
            wrapper_obj.to_str().unwrap(),
            imported_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "linked binary should be produced: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run failed");
    assert!(
        run.status.success(),
        "USE-renamed call target binary failed: {:?}\nstderr:{}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("IMPORTED"),
        "USE-renamed call should target the imported procedure, not the local collision: {}",
        stdout
    );
    assert!(
        !stdout.contains("LOCAL"),
        "USE-renamed call should not dispatch to the local same-named procedure: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn local_char_parameter_array_elements_preserve_runtime_bytes() {
    let src = write_program(
        "module m\n  implicit none\ncontains\n  subroutine identify(command_name, type_flag)\n    character(len=*), intent(in) :: command_name\n    logical, intent(in) :: type_flag\n    if (is_builtin_command(command_name)) then\n      if (type_flag) then\n        print *, 'builtin'\n      else\n        print *, trim(command_name)\n      end if\n    else\n      print *, 'missing'\n    end if\n  end subroutine\n\n  function is_builtin_command(command_name) result(is_builtin)\n    character(len=*), intent(in) :: command_name\n    logical :: is_builtin\n    character(len=16), parameter :: builtins(4) = [ &\n      'cd              ', 'pwd             ', 'echo            ', 'printf          ' ]\n    integer :: i\n    is_builtin = .false.\n    do i = 1, size(builtins)\n      if (trim(command_name) == trim(builtins(i))) then\n        is_builtin = .true.\n        return\n      end if\n    end do\n  end function\nend module\n\nprogram p\n  use m\n  implicit none\n  character(len=256) :: command_name\n  command_name = 'echo'\n  call identify(command_name, .true.)\n  call identify(command_name, .false.)\nend program\n",
        "f90",
    );
    let out = unique_path("char_param_array", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("char parameter array compile spawn failed");
    assert!(
        compile.status.success(),
        "char parameter array compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "char parameter array runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    let lines: Vec<_> = stdout.lines().map(str::trim).collect();
    assert_eq!(
        lines,
        vec!["builtin", "echo"],
        "unexpected local char parameter array runtime output"
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fixed_char_array_element_actual_to_char_dummy_runs() {
    let src = write_program(
        "subroutine install_single_trap(signal_name, command)\n  implicit none\n  character(len=*), intent(in) :: signal_name, command\n  print *, trim(signal_name), trim(command)\nend subroutine\n\nsubroutine parse_signal_list(signals, signal_names, count)\n  implicit none\n  character(len=*), intent(in) :: signals\n  character(len=32), intent(out) :: signal_names(20)\n  integer, intent(out) :: count\n  count = 1\n  signal_names(1) = signals\nend subroutine\n\nsubroutine install_trap(signals, command)\n  implicit none\n  character(len=*), intent(in) :: signals, command\n  character(len=32) :: signal_names(20)\n  integer :: signal_count, i\n  call parse_signal_list(signals, signal_names, signal_count)\n  do i = 1, signal_count\n    call install_single_trap(signal_names(i), command)\n  end do\nend subroutine\n\nprogram p\n  implicit none\n  call install_trap('INT', 'echo')\nend program\n",
        "f90",
    );
    let out = unique_path("char_array_element_actual", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("char array element actual compile spawn failed");
    assert!(
        compile.status.success(),
        "char array element actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "char array element actual runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("INT") && stdout.contains("echo"),
        "unexpected fixed char array element actual output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn external_optional_dummy_absent_still_reserves_slot_before_hidden_char_lengths() {
    let dir = unique_dir("optional_hidden_char_len");
    let mod_src = write_program_in(
        &dir,
        "m.f90",
        "module m\ncontains\n  subroutine foo(name, value, value_length)\n    character(len=*), intent(in) :: name, value\n    integer, intent(in), optional :: value_length\n    integer :: n\n    if (present(value_length)) then\n      n = value_length\n    else\n      n = len_trim(value)\n    end if\n    print *, trim(name), n\n  end subroutine foo\nend module\n",
    );
    let user_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use m\n  implicit none\n  character(len=8) :: s\n  s = 'false'\n  call inner()\ncontains\n  subroutine inner()\n    call foo('COLUMNS', trim(s))\n  end subroutine inner\nend program\n",
    );

    let mod_obj = dir.join("m.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "optional module compile failed: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let user_obj = dir.join("main.o");
    let compile_user = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            user_src.to_str().unwrap(),
            "-o",
            user_obj.to_str().unwrap(),
        ])
        .output()
        .expect("user compile spawn failed");
    assert!(
        compile_user.status.success(),
        "optional caller compile failed: {}",
        String::from_utf8_lossy(&compile_user.stderr)
    );

    let out = dir.join("p.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            user_obj.to_str().unwrap(),
            mod_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "optional caller link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("optional caller run failed");
    assert!(
        run.status.success(),
        "optional caller run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("COLUMNS") && stdout.contains('5'),
        "optional caller should fall back to len_trim(value): {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn keyword_actual_preserves_skipped_optional_slot() {
    let src = write_program(
        "module m\n  implicit none\n  type :: t\n    integer :: a = 0\n    logical :: b = .false.\n    integer :: c = 0\n  end type\ncontains\n  subroutine foo(x, a, b, c)\n    type(t), intent(inout) :: x\n    integer, intent(in) :: a\n    logical, intent(in), optional :: b\n    integer, intent(in), optional :: c\n    x%a = a\n    if (present(b)) then\n      x%b = b\n    else\n      x%b = .false.\n    end if\n    if (present(c)) then\n      x%c = c\n    else\n      x%c = -1\n    end if\n  end subroutine foo\nend module m\nprogram p\n  use m\n  implicit none\n  type(t) :: x\n  call foo(x, 11, c=77)\n  if (x%a /= 11) error stop 1\n  if (x%b) error stop 2\n  if (x%c /= 77) error stop 3\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("keyword_optional_gap", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("keyword optional gap compile failed to spawn");
    assert!(
        compile.status.success(),
        "keyword optional gap compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("keyword optional gap run failed");
    assert!(
        run.status.success(),
        "keyword optional gap run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected keyword optional gap output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn use_renamed_subroutine_call_preserves_optional_slot_and_hidden_char_lengths() {
    let src = write_program(
        "module m\ncontains\n  subroutine sink(name, value, value_length)\n    character(len=*), intent(in) :: name, value\n    integer, intent(in), optional :: value_length\n    write(*,'(A,L1)') 'PRESENT=', present(value_length)\n    write(*,'(A,I0)') 'NLEN=', len(name)\n    write(*,'(A,I0)') 'VLEN=', len(value)\n    write(*,'(A)') 'PAIR=' // trim(name) // ':' // trim(value)\n  end subroutine sink\nend module m\nprogram p\n  use m, only: alias_sink => sink\n  implicit none\n  character(len=8) :: a, b\n  a = 'X'\n  b = 'YZ'\n  call alias_sink(trim(a), trim(b))\nend program p\n",
        "f90",
    );
    let out = unique_path("use_rename_hidden_len", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("use-rename hidden-len compile failed to spawn");
    assert!(
        compile.status.success(),
        "use-rename hidden-len compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("use-rename hidden-len run failed");
    assert!(
        run.status.success(),
        "use-rename hidden-len run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("PRESENT=F"),
        "renamed call should keep the optional argument absent: {}",
        stdout
    );
    assert!(
        stdout.contains("NLEN=1"),
        "renamed call should preserve the first hidden character length: {}",
        stdout
    );
    assert!(
        stdout.contains("VLEN=2"),
        "renamed call should preserve the second hidden character length: {}",
        stdout
    );
    assert!(
        stdout.contains("PAIR=X:YZ"),
        "renamed call should preserve the trimmed character payloads: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn inquire_file_with_sparse_optional_string_outputs_runs() {
    let src = write_program(
        "program p\n  implicit none\n  logical :: exists\n  character(len=16) :: access, action\n  inquire(file='fortsh_missing_config_marker', exist=exists, access=access, action=action)\n  print *, trim(access), trim(action)\nend program\n",
        "f90",
    );
    let out = unique_path("inquire_sparse_optional", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "sparse INQUIRE compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "sparse INQUIRE runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.matches("UNDEFINED").count() >= 2,
        "sparse INQUIRE should populate ACCESS/ACTION without crashing: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn open_with_newunit_and_iostat_uses_keyword_specs() {
    let dir = unique_dir("open_newunit_iostat");
    let input = dir.join("input.txt");
    fs::write(&input, "hello\n").expect("write input");
    let src = write_program_in(
        &dir,
        "main.f90",
        &format!(
            "program p\n  implicit none\n  integer :: u, ios\n  character(len=16) :: line\n  u = -77\n  ios = -88\n  open(newunit=u, file='{}', status='old', action='read', iostat=ios)\n  if (ios /= 0) error stop 1\n  if (u == -77) error stop 2\n  read(u, '(a)', iostat=ios) line\n  if (ios /= 0) error stop 3\n  if (trim(line) /= 'hello') error stop 4\n  close(u)\n  print *, 'ok'\nend program\n",
            input.display()
        ),
    );
    let out = dir.join("open_newunit_iostat.bin");
    let compile = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "OPEN with NEWUNIT/IOSTAT compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "OPEN with NEWUNIT/IOSTAT runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "OPEN with NEWUNIT/IOSTAT should assign the new unit and read the file: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn char_concat_actual_to_assumed_len_dummy_runs() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=16) :: home\n  home = 'abc'\n  call show(trim(home)//'/.fortshrc')\ncontains\n  subroutine show(path)\n    character(len=*), intent(in) :: path\n    print *, trim(path)\n  end subroutine show\nend program\n",
        "f90",
    );
    let out = unique_path("char_concat_actual", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "character concat actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "character concat actual runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("abc/.fortshrc"),
        "character concat actual should preserve both sides of the string: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn if_else_assignment_to_dummy_argument_runs() {
    let src = write_program(
        "module m\ncontains\n  subroutine set_flag(flag)\n    integer, intent(out) :: flag\n    if (.true.) then\n      flag = 7\n    else\n      flag = -1\n    end if\n  end subroutine\nend module\n\nprogram main\n  use m\n  implicit none\n  integer :: flag\n  call set_flag(flag)\n  print *, flag\nend program\n",
        "f90",
    );
    let out = unique_path("if_else_dummy_assign", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "if/else dummy assignment compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "if/else dummy assignment runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains('7'),
        "if/else dummy assignment should write through the caller's storage: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn char_parameter_constants_preserve_bytes_and_concat() {
    let src = write_program(
        "module version_matrix\n  implicit none\n  character(len=*), parameter :: mod_star = '1.7.0'\ncontains\n  subroutine print_mod_star()\n    print '(a)', mod_star\n  end subroutine\n  subroutine print_mod_star_concat()\n    print '(a)', 'fortsh ' // mod_star\n  end subroutine\nend module\n\nprogram main\n  use version_matrix\n  implicit none\n  character(len=*), parameter :: local_star = '2.3.4'\n  print '(a)', local_star\n  call print_mod_star()\n  call print_mod_star_concat()\nend program\n",
        "f90",
    );
    let out = unique_path("char_param_matrix", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("char parameter matrix compile spawn failed");
    assert!(
        compile.status.success(),
        "char parameter matrix compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "char parameter matrix runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    let lines: Vec<_> = stdout.lines().map(str::trim).collect();
    assert_eq!(
        lines,
        vec!["2.3.4", "1.7.0", "fortsh 1.7.0"],
        "unexpected char parameter runtime output"
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn char_parameter_round_trips_through_amod_import() {
    let dir = unique_dir("char_param_amod");
    let mod_src = write_program_in(
        &dir,
        "version_mod.f90",
        "module version_mod\n  implicit none\n  character(len=*), parameter :: fortsh_version = '1.7.0'\nend module\n",
    );
    let user_src = write_program_in(
        &dir,
        "user.f90",
        "program p\n  use version_mod, only: fortsh_version\n  implicit none\n  print '(a)', fortsh_version\nend program\n",
    );
    let mod_obj = dir.join("version_mod.o");
    let user_obj = dir.join("user.o");
    let out = dir.join("user_bin");

    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("char parameter module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "char parameter module compile failed: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let compile_user = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            user_src.to_str().unwrap(),
            "-o",
            user_obj.to_str().unwrap(),
        ])
        .output()
        .expect("char parameter user compile spawn failed");
    assert!(
        compile_user.status.success(),
        "char parameter user compile failed: {}",
        String::from_utf8_lossy(&compile_user.stderr)
    );

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            mod_obj.to_str().unwrap(),
            user_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("char parameter link spawn failed");
    assert!(
        link.status.success(),
        "char parameter link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "char parameter user runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "1.7.0",
        "unexpected imported char parameter output"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn char_intrinsics_and_transfer_lower_without_raw_symbols() {
    let src = write_program(
        "module m\n  use iso_c_binding, only: c_funptr, c_intptr_t\ncontains\n  subroutine s(buf, mask, ok)\n    character(len=:), allocatable, intent(inout) :: buf\n    logical, intent(in) :: mask\n    logical, intent(out) :: ok\n    type(c_funptr) :: sig_ign\n    if (allocated(buf)) then\n      ok = lgt(trim(buf), 'a')\n    else\n      ok = .false.\n    end if\n    ok = ok .or. any(buf(1:1) == ['!', '?'])\n    buf = merge(buf // new_line('a'), '?', mask)\n    sig_ign = transfer(1_c_intptr_t, sig_ign)\n  end subroutine s\nend module m\n",
        "f90",
    );
    let out = unique_path("char_intrinsics_link", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("char intrinsic compile failed to spawn");
    assert!(
        compile.status.success(),
        "char intrinsic compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&out);
    assert!(
        undefined.iter().any(|sym| sym == "_afs_string_allocated"),
        "deferred-char ALLOCATED() should lower to the string runtime: {:?}",
        undefined
    );
    assert!(
        undefined.iter().any(|sym| sym == "_afs_lgt"),
        "LGT should lower to the string runtime: {:?}",
        undefined
    );
    assert!(
        !undefined.iter().any(|sym| {
            matches!(
                sym.as_str(),
                "_allocated" | "_any" | "_merge" | "_new_line" | "_transfer" | "_lgt"
            )
        }),
        "char/link intrinsics should not escape as raw symbols: {:?}",
        undefined
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn deferred_char_allocatable_dummy_uses_descriptor_abi() {
    let src = write_program(
        "module m\ncontains\n  subroutine grow(buf, cap, content_len)\n    character(len=:), allocatable, intent(inout) :: buf\n    integer, intent(inout) :: cap\n    integer, intent(in) :: content_len\n    character(len=:), allocatable :: tmp\n    integer :: new_cap\n    new_cap = cap * 2\n    allocate(character(len=new_cap) :: tmp)\n    if (content_len > 0) tmp(1:content_len) = buf(1:content_len)\n    call move_alloc(tmp, buf)\n    cap = new_cap\n  end subroutine\nend module\n",
        "f90",
    );
    let out = unique_path("deferred_char_dummy", "s");
    let compile = Command::new(compiler("armfortas"))
        .args(["-S", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "deferred-length allocatable character dummy should lower cleanly: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let asm = std::fs::read_to_string(&out).expect("cannot read deferred-char dummy assembly");
    assert!(
        asm.contains("bl _afs_move_alloc_string"),
        "MOVE_ALLOC on deferred-length character dummies should call the string runtime: {}",
        asm
    );
    assert!(
        !asm.contains("bl _move_alloc"),
        "deferred-length character MOVE_ALLOC should not escape as a raw external call: {}",
        asm
    );
    assert!(
        !asm.contains("bl _buf"),
        "substringing a deferred-length dummy should not lower as a fake function call: {}",
        asm
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn deferred_char_allocatable_array_dummy_whole_and_element_assignment_runs() {
    let src = write_program(
        "module m\ncontains\n  subroutine fill(tokens)\n    character(len=:), allocatable, intent(out) :: tokens(:)\n    allocate(character(len=32) :: tokens(2))\n    tokens = ''\n    tokens(1) = 'hello'\n    tokens(2) = 'world'\n  end subroutine\nend module\nprogram p\n  use m, only: fill\n  implicit none\n  character(len=:), allocatable :: tokens(:)\n  call fill(tokens)\n  print *, trim(tokens(1)), trim(tokens(2))\nend program\n",
        "f90",
    );
    let out = unique_path("deferred_char_array_dummy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "deferred-length allocatable character array dummy should compile and link: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "deferred-length allocatable character array dummy binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("hello") && stdout.contains("world"),
        "deferred char array dummy assignments should preserve element text: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fixed_len_allocatable_char_array_dummy_round_trips_through_amod_import_and_runs() {
    let dir = unique_dir("fixed_len_char_array_dummy_amod");
    let mod_src = write_program_in(
        &dir,
        "m.f90",
        "module m\ncontains\n  subroutine fill(tokens, num_tokens, expanded_tokens, expanded_count)\n    character(len=*), intent(in) :: tokens(:)\n    integer, intent(in) :: num_tokens\n    character(len=32), allocatable, intent(out) :: expanded_tokens(:)\n    integer, intent(out) :: expanded_count\n    integer :: i\n    expanded_count = num_tokens\n    allocate(expanded_tokens(expanded_count))\n    do i = 1, expanded_count\n      expanded_tokens(i) = tokens(i)\n    end do\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use m, only: fill\n  implicit none\n  character(len=32) :: tokens(2)\n  character(len=32), allocatable :: expanded_tokens(:)\n  integer :: expanded_count\n  tokens(1) = 'echo'\n  tokens(2) = 'foo[1]'\n  call fill(tokens, 2, expanded_tokens, expanded_count)\n  print *, 'COUNT=', expanded_count\n  print *, 'TOK1=<' // trim(expanded_tokens(1)) // '>'\n  print *, 'TOK2=<' // trim(expanded_tokens(2)) // '>'\nend program\n",
    );

    let mod_obj = dir.join("m.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "fixed-length allocatable char array module should compile: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("main compile spawn failed");
    assert!(
        compile_main.status.success(),
        "fixed-length allocatable char array consumer should compile through .amod: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("fixed_len_char_array_dummy_amod.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            mod_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "fixed-length allocatable char array .amod link should succeed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run failed");
    assert!(
        run.status.success(),
        "fixed-length allocatable char array .amod binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("COUNT= 2") || stdout.contains("COUNT=2"),
        "expected element count to survive round-trip: {}",
        stdout
    );
    assert!(
        stdout.contains("TOK1=<echo>") && stdout.contains("TOK2=<foo[1]>"),
        "fixed-length allocatable char array dummy should preserve element text across .amod import: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn use_renamed_procedure_call_uses_remote_symbol() {
    let dir = unique_dir("use_rename_proc");
    let mod_src = write_program_in(
        &dir,
        "m.f90",
        "module m\ncontains\n  subroutine set_shell_variable()\n  end subroutine set_shell_variable\nend module m\n",
    );
    let user_src = write_program_in(
        &dir,
        "user.f90",
        "module user\ncontains\n  subroutine run()\n    use m, only: var_set_shell_variable => set_shell_variable\n    call var_set_shell_variable()\n  end subroutine run\nend module user\n",
    );

    let mod_obj = dir.join("m.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("rename module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "rename source module should compile: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let user_obj = dir.join("user.o");
    let compile_user = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            user_src.to_str().unwrap(),
            "-o",
            user_obj.to_str().unwrap(),
        ])
        .output()
        .expect("rename user compile spawn failed");
    assert!(
        compile_user.status.success(),
        "USE-renamed procedure call should compile: {}",
        String::from_utf8_lossy(&compile_user.stderr)
    );

    let undefined = undefined_symbols(&user_obj);
    assert!(
        undefined
            .iter()
            .any(|sym| sym == "_afs_modproc_m_set_shell_variable"),
        "USE rename should call the imported procedure symbol: {:?}",
        undefined
    );
    assert!(
        !undefined.iter().any(|sym| sym == "_var_set_shell_variable"),
        "USE rename should not lower to the local alias as a link symbol: {:?}",
        undefined
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn linked_binary_carries_uuid_and_launches() {
    let dir = unique_dir("linked_binary_uuid");
    let src = write_program_in(
        &dir,
        "hello.f90",
        "program hello\n  print *, 42\nend program hello\n",
    );

    let exe = dir.join("hello.bin");
    let compile = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([src.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("hello compile spawn failed");
    assert!(
        compile.status.success(),
        "linked hello should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let otool = Command::new("otool")
        .args(["-l", exe.to_str().unwrap()])
        .output()
        .expect("otool spawn failed");
    assert!(
        otool.status.success(),
        "otool should inspect linked hello: {}",
        String::from_utf8_lossy(&otool.stderr)
    );
    let load_commands = String::from_utf8_lossy(&otool.stdout);
    assert!(
        load_commands.contains("LC_UUID"),
        "linked hello should carry LC_UUID so dyld accepts it:\n{}",
        load_commands
    );

    let run = Command::new(&exe)
        .current_dir(&dir)
        .output()
        .expect("hello run spawn failed");
    assert!(
        run.status.success(),
        "linked hello should launch successfully:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn same_named_module_helpers_link_without_colliding() {
    let dir = unique_dir("module_helper_link_names");
    let mod_a = write_program_in(
        &dir,
        "mod_a.f90",
        "module mod_a\ncontains\n  subroutine helper()\n    print *, 11\n  end subroutine helper\n\n  subroutine run_a()\n    call helper()\n  end subroutine run_a\nend module mod_a\n",
    );
    let mod_b = write_program_in(
        &dir,
        "mod_b.f90",
        "module mod_b\ncontains\n  subroutine helper()\n    print *, 22\n  end subroutine helper\n\n  subroutine run_b()\n    call helper()\n  end subroutine run_b\nend module mod_b\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use mod_a, only: run_a\n  use mod_b, only: run_b\n  call run_a()\n  call run_b()\nend program p\n",
    );

    let mod_a_obj = dir.join("mod_a.o");
    let compile_a = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_a.to_str().unwrap(),
            "-o",
            mod_a_obj.to_str().unwrap(),
        ])
        .output()
        .expect("mod_a compile spawn failed");
    assert!(
        compile_a.status.success(),
        "mod_a should compile: {}",
        String::from_utf8_lossy(&compile_a.stderr)
    );

    let mod_b_obj = dir.join("mod_b.o");
    let compile_b = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            mod_b.to_str().unwrap(),
            "-o",
            mod_b_obj.to_str().unwrap(),
        ])
        .output()
        .expect("mod_b compile spawn failed");
    assert!(
        compile_b.status.success(),
        "mod_b should compile: {}",
        String::from_utf8_lossy(&compile_b.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("main compile spawn failed");
    assert!(
        compile_main.status.success(),
        "main should compile: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("helpers.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            mod_a_obj.to_str().unwrap(),
            mod_b_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("helper link spawn failed");
    assert!(
        link.status.success(),
        "same-named module helpers should link cleanly: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn contained_helpers_link_without_cross_object_internal_symbol_collisions() {
    let dir = unique_dir("contained_helper_link_names");
    let mod_a = write_program_in(
        &dir,
        "mod_a.f90",
        "module mod_a\ncontains\n  subroutine run_a()\n    implicit none\n    call helper()\n  contains\n    subroutine helper()\n      print *, 11\n    end subroutine helper\n  end subroutine run_a\nend module mod_a\n",
    );
    let mod_b = write_program_in(
        &dir,
        "mod_b.f90",
        "module mod_b\ncontains\n  subroutine run_b()\n    implicit none\n    call helper()\n  contains\n    subroutine helper()\n      print *, 22\n    end subroutine helper\n  end subroutine run_b\nend module mod_b\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use mod_a, only: run_a\n  use mod_b, only: run_b\n  call run_a()\n  call run_b()\nend program p\n",
    );

    let mod_a_obj = dir.join("mod_a.o");
    let compile_a = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_a.to_str().unwrap(),
            "-o",
            mod_a_obj.to_str().unwrap(),
        ])
        .output()
        .expect("mod_a compile spawn failed");
    assert!(
        compile_a.status.success(),
        "mod_a should compile: {}",
        String::from_utf8_lossy(&compile_a.stderr)
    );

    let mod_b_obj = dir.join("mod_b.o");
    let compile_b = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            mod_b.to_str().unwrap(),
            "-o",
            mod_b_obj.to_str().unwrap(),
        ])
        .output()
        .expect("mod_b compile spawn failed");
    assert!(
        compile_b.status.success(),
        "mod_b should compile: {}",
        String::from_utf8_lossy(&compile_b.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("main compile spawn failed");
    assert!(
        compile_main.status.success(),
        "main should compile: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("contained_helpers.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            mod_a_obj.to_str().unwrap(),
            mod_b_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("contained helper link spawn failed");
    assert!(
        link.status.success(),
        "contained helpers in different objects should link cleanly: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn program_internal_char_helper_assignment_uses_internal_symbol() {
    let dir = unique_dir("program_internal_char_helper");
    let src = write_program_in(
        &dir,
        "p.f90",
        "program p\n  implicit none\n  character(len=16) :: x\n  x = helper('a')\ncontains\n  function helper(v) result(out)\n    character(len=*), intent(in) :: v\n    character(len=16) :: out\n    out = v\n  end function helper\nend program p\n",
    );

    let obj = dir.join("p.o");
    let compile = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args(["-c", src.to_str().unwrap(), "-o", obj.to_str().unwrap()])
        .output()
        .expect("program compile spawn failed");
    assert!(
        compile.status.success(),
        "program-contained character helper should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&obj);
    assert!(
        !undefined.iter().any(|sym| sym == "_helper"),
        "program-contained character helper should not escape as a raw external symbol: {:?}",
        undefined
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn runtime_sized_character_function_result_compiles_and_runs() {
    let dir = unique_dir("runtime_char_function_result");
    let module_src = write_program_in(
        &dir,
        "m.f90",
        "module m\ncontains\n  function normalize(input) result(output)\n    character(len=*), intent(in) :: input\n    character(len=len(input)) :: output\n    integer :: i, j\n    output = ''\n    i = 1\n    j = 1\n    do while (i <= len_trim(input))\n      if (input(i:i) == char(10)) then\n        i = i + 1\n        cycle\n      end if\n      output(j:j) = input(i:i)\n      i = i + 1\n      j = j + 1\n    end do\n  end function normalize\nend module m\n",
    );
    let program_src = write_program_in(
        &dir,
        "p.f90",
        "program p\n  use m, only: normalize\n  implicit none\n  character(len=3) :: input, output\n  input = 'a' // char(10) // 'b'\n  output = normalize(input)\n  if (output /= 'ab ') error stop 1\n  print *, trim(output)\nend program p\n",
    );
    let out = dir.join("p.out");

    let compile = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            module_src.to_str().unwrap(),
            program_src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("runtime-sized char function compile spawn failed");
    assert!(
        compile.status.success(),
        "runtime-sized char function result should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "runtime-sized char function result should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ab"),
        "unexpected runtime-sized char function output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn formatted_write_of_concat_string_runs() {
    let src = write_program(
        "program p\n  use iso_fortran_env, only: output_unit\n  implicit none\n  write(output_unit, '(a)') 'fortsh ' // '1.7.0'\nend program\n",
        "f90",
    );
    let out = unique_path("formatted_concat_write", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("formatted concat write compile spawn failed");
    assert!(
        compile.status.success(),
        "formatted concat write should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "formatted concat write should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("fortsh 1.7.0"),
        "unexpected formatted concat write output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn formatted_write_of_concat_with_internal_char_function_runs() {
    let src = write_program(
        "program p\n  implicit none\n  write(*, '(a)') 'x=' // get_s()\ncontains\n  function get_s() result(str)\n    character(len=20) :: str\n    str = 'ok'\n  end function\nend program\n",
        "f90",
    );
    let out = unique_path("formatted_concat_internal_char", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("formatted concat internal-char compile spawn failed");
    assert!(
        compile.status.success(),
        "formatted concat internal-char should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "formatted concat internal-char should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("x=ok"),
        "unexpected formatted concat internal-char output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn contained_subroutine_forwards_derived_dummy_by_ref() {
    let src = write_program(
        "module m\n  implicit none\n  type :: t\n    integer :: pad(2000) = 0\n    integer :: x = 0\n  end type\ncontains\n  subroutine setx(a)\n    type(t), intent(inout) :: a\n    a%x = 7\n  end subroutine\nend module\n\nprogram p\n  use m\n  implicit none\n  type(t), allocatable :: v\n  allocate(v)\n  call init(v)\n  print *, v%x\ncontains\n  subroutine init(a)\n    type(t), intent(out) :: a\n    call setx(a)\n  end subroutine\nend program\n",
        "f90",
    );
    let out = unique_path("contained_forward_dt_dummy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("contained derived-dummy forward compile spawn failed");
    assert!(
        compile.status.success(),
        "contained derived-dummy forward should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "contained derived-dummy forward should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("7"),
        "unexpected contained derived-dummy forward output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn default_integer_system_clock_runs_without_runtime_abi_crash() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: count, rate, max_count\n  call system_clock(count, rate, max_count)\n  if (rate == 0) error stop 1\n  if (max_count == 0) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("system_clock_default_integer", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("default integer system_clock compile spawn failed");
    assert!(
        compile.status.success(),
        "default integer system_clock should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "default integer system_clock should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected default integer system_clock output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn saved_derived_global_after_small_globals_keeps_descriptor_alignment() {
    let src = write_program(
        "module m\n  implicit none\n  logical, save :: flag1 = .false.\n  logical, save :: flag2 = .false.\n  logical, save :: flag3 = .false.\n  type :: history_t\n    character(len=16), allocatable :: lines(:)\n    integer :: count = 0\n    integer :: current = 0\n    logical :: initialized = .false.\n  end type\n  type(history_t), save :: history\ncontains\n  subroutine init_history()\n    if (.not. history%initialized) then\n      allocate(history%lines(4))\n      history%lines = ''\n      history%count = 1\n      history%initialized = .true.\n    end if\n    print *, history%count, size(history%lines)\n  end subroutine\nend module\nprogram p\n  use m\n  call init_history()\nend program\n",
        "f90",
    );
    let out = unique_path("saved_derived_global_alignment", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("saved derived global alignment compile spawn failed");
    assert!(
        compile.status.success(),
        "saved derived global alignment should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "saved derived global alignment should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("1 4") || stdout.contains("1  4"),
        "unexpected saved derived global alignment output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn derived_local_with_allocatable_component_and_trailing_scalar_runs() {
    let src = write_program(
        "program p\n  implicit none\n  type :: t\n    integer, allocatable :: a(:)\n    integer :: n\n  end type\n  type(t) :: x\n  allocate(x%a(1))\n  x%n = 7\n  x%a(1) = 17\n  if (.not. allocated(x%a)) stop 1\n  if (size(x%a) /= 1) stop 2\n  if (x%n /= 7) stop 3\n  if (x%a(1) /= 17) stop 4\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("derived_local_alloc_comp_scalar_tail", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived local allocatable component compile spawn failed");
    assert!(
        compile.status.success(),
        "derived local allocatable component should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "derived local allocatable component should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected derived local allocatable component output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_array_component_passes_descriptor_to_dummy() {
    let src = write_program(
        "program p\n  implicit none\n  type :: box_t\n    integer, allocatable :: xs(:)\n  end type\n  type(box_t) :: box\n  allocate(box%xs(3))\n  box%xs = [1, 2, 3]\n  call check(box%xs)\n  print *, 'ok'\ncontains\n  subroutine check(xs)\n    integer, intent(in) :: xs(:)\n    if (size(xs) /= 3) error stop 1\n    if (xs(2) /= 2) error stop 2\n  end subroutine check\nend program p\n",
        "f90",
    );
    let out = unique_path("alloc_component_descriptor_dummy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable component descriptor compile spawn failed");
    assert!(
        compile.status.success(),
        "allocatable component descriptor program should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "allocatable component descriptor program should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected allocatable component descriptor output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_char_array_component_passes_descriptor_to_dummy() {
    let src = write_program(
        "program p\n  implicit none\n  type :: box_t\n    character(len=:), allocatable :: tokens(:)\n    integer :: num_tokens = 0\n  end type\n  type(box_t) :: box\n  call fill_tokens(box%tokens, box%num_tokens)\n  if (.not. allocated(box%tokens)) error stop 1\n  if (box%num_tokens /= 2) error stop 2\n  if (size(box%tokens) /= 2) error stop 3\n  if (trim(box%tokens(1)) /= 'echo') error stop 4\n  if (trim(box%tokens(2)) /= 'hello') error stop 5\n  print *, 'ok'\ncontains\n  subroutine fill_tokens(tokens, num_tokens)\n    character(len=:), allocatable, intent(out) :: tokens(:)\n    integer, intent(out) :: num_tokens\n    character(len=16), allocatable :: temp_tokens(:)\n    integer :: i\n    num_tokens = 2\n    allocate(temp_tokens(num_tokens))\n    temp_tokens(1) = 'echo'\n    temp_tokens(2) = 'hello'\n    allocate(character(len=16) :: tokens(num_tokens))\n    do i = 1, num_tokens\n      tokens(i) = temp_tokens(i)\n    end do\n    deallocate(temp_tokens)\n  end subroutine fill_tokens\nend program p\n",
        "f90",
    );
    let out = unique_path("alloc_component_char_descriptor_dummy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable char component descriptor compile spawn failed");
    assert!(
        compile.status.success(),
        "allocatable char component descriptor program should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "allocatable char component descriptor program should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected allocatable char component descriptor output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn derived_array_dummy_uses_real_element_stride() {
    let src = write_program(
        "module m\n  implicit none\n  integer, parameter :: max_token_len = 16\n  type :: token_t\n    integer :: token_type\n    character(len=max_token_len) :: value\n    integer :: value_length = 0\n    integer :: start_pos = 0\n    integer :: end_pos = 0\n    integer :: line = 1\n    logical :: quoted = .false.\n    logical :: escaped = .false.\n    integer :: quote_type = 0\n  end type token_t\ncontains\n  subroutine add_token(tokens, num_tokens, tok_type, value)\n    type(token_t), intent(inout) :: tokens(:)\n    integer, intent(inout) :: num_tokens\n    integer, intent(in) :: tok_type\n    character(len=*), intent(in) :: value\n    if (num_tokens < size(tokens)) then\n      num_tokens = num_tokens + 1\n      tokens(num_tokens)%token_type = tok_type\n      tokens(num_tokens)%value = value\n      tokens(num_tokens)%value_length = len_trim(value)\n    end if\n  end subroutine add_token\nend module m\nprogram p\n  use m\n  implicit none\n  type(token_t), allocatable :: tokens(:)\n  integer :: num_tokens\n  allocate(tokens(4))\n  num_tokens = 0\n  call add_token(tokens, num_tokens, 1, 'echo')\n  call add_token(tokens, num_tokens, 2, 'ok')\n  if (num_tokens /= 2) error stop 1\n  if (tokens(1)%token_type /= 1) error stop 2\n  if (trim(tokens(1)%value) /= 'echo') error stop 3\n  if (tokens(1)%value_length /= 4) error stop 4\n  if (tokens(2)%token_type /= 2) error stop 5\n  if (trim(tokens(2)%value) /= 'ok') error stop 6\n  if (tokens(2)%value_length /= 2) error stop 7\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("derived_array_dummy_stride", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived array dummy stride compile spawn failed");
    assert!(
        compile.status.success(),
        "derived array dummy stride program should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "derived array dummy stride program should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected derived array dummy stride output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn scalar_substring_actual_passes_runtime_len_to_len_star_dummy() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=16) :: s\n  integer :: n\n  s = 'echo'\n  n = 4\n  call check(s(1:n))\n  print *, 'ok'\ncontains\n  subroutine check(value)\n    character(len=*), intent(in) :: value\n    if (len(value) /= 4) error stop 1\n    if (trim(value) /= 'echo') error stop 2\n  end subroutine check\nend program p\n",
        "f90",
    );
    let out = unique_path("scalar_substring_len_star_dummy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("scalar substring len-star dummy compile spawn failed");
    assert!(
        compile.status.success(),
        "scalar substring len-star dummy program should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "scalar substring len-star dummy program should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected scalar substring len-star dummy output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn parameter_actual_to_by_ref_dummy_materializes_temp_slot() {
    let src = write_program(
        "program p\n  implicit none\n  integer, parameter :: eof_token = 6\n  call check(eof_token)\n  print *, 'ok'\ncontains\n  subroutine check(value)\n    integer, intent(in) :: value\n    if (value /= 6) error stop 1\n  end subroutine check\nend program p\n",
        "f90",
    );
    let out = unique_path("parameter_by_ref_dummy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("parameter by-ref dummy compile spawn failed");
    assert!(
        compile.status.success(),
        "parameter by-ref dummy program should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "parameter by-ref dummy program should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected parameter by-ref dummy output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn pointer_function_result_can_forward_other_pointer_call() {
    let src = write_program(
        "module m\n  implicit none\n  type :: node_t\n    integer :: value = 0\n  end type node_t\ncontains\n  function make_node(n) result(node)\n    integer, intent(in) :: n\n    type(node_t), pointer :: node\n    allocate(node)\n    node%value = n\n  end function make_node\n\n  function forward_node(n) result(node)\n    integer, intent(in) :: n\n    type(node_t), pointer :: node\n    node => make_node(n)\n  end function forward_node\nend module m\n\nprogram p\n  use m\n  implicit none\n  type(node_t), pointer :: root\n  root => forward_node(42)\n  if (.not. associated(root)) error stop 1\n  if (root%value /= 42) error stop 2\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("pointer_result_forwarding", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("pointer result forwarding compile spawn failed");
    assert!(
        compile.status.success(),
        "pointer result forwarding should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "pointer result forwarding should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected pointer result forwarding output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn deferred_character_pointer_function_result_compiles_and_runs() {
    let src = write_program(
        "module m\ncontains\n  function maybe_ptr(flag) result(ptr)\n    logical, intent(in) :: flag\n    character(:), pointer :: ptr\n    character(len=4), target, save :: pool = 'okay'\n    if (flag) then\n      ptr => pool(1:4)\n    else\n      ptr => null()\n    end if\n  end function maybe_ptr\nend module m\n\nprogram p\n  use m, only: maybe_ptr\n  implicit none\n  character(len=:), allocatable :: s\n  s = maybe_ptr(.true.)\n  if (s /= 'okay') error stop 1\n  print *, trim(s)\nend program p\n",
        "f90",
    );
    let out = unique_path("deferred_char_pointer_result", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("deferred char pointer result compile spawn failed");
    assert!(
        compile.status.success(),
        "deferred char pointer function result should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "deferred char pointer function result should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("okay"),
        "unexpected deferred char pointer result output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_result_helper_assignment_uses_resolved_symbol() {
    let dir = unique_dir("alloc_result_helper_symbol");
    let src = write_program_in(
        &dir,
        "m.f90",
        "module m\ncontains\n  function helper(x) result(y)\n    character(len=*), intent(in) :: x\n    character(len=:), allocatable :: y\n    y = trim(x)\n  end function helper\n\n  function run(x) result(y)\n    character(len=*), intent(in) :: x\n    character(len=:), allocatable :: y\n    y = helper(x)\n  end function run\nend module m\n",
    );

    let obj = dir.join("m.o");
    let compile = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args(["-c", src.to_str().unwrap(), "-o", obj.to_str().unwrap()])
        .output()
        .expect("module compile spawn failed");
    assert!(
        compile.status.success(),
        "allocatable-result helper source should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&obj);
    assert!(
        !undefined.iter().any(|sym| sym == "_helper"),
        "same-file allocatable-result helper should not lower to a raw external symbol: {:?}",
        undefined
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocatable_derived_array_function_result_assignment_runs() {
    let src = write_program(
        "module m\n  implicit none\n  type :: string_t\n    character(len=:), allocatable :: str\n  end type\ncontains\n  function clone(src, count) result(body)\n    type(string_t), intent(in) :: src(:)\n    integer, intent(in) :: count\n    type(string_t), allocatable :: body(:)\n    integer :: j\n    allocate(body(count))\n    do j = 1, count\n      body(j)%str = src(j)%str\n    end do\n  end function clone\nend module m\n\nprogram p\n  use m\n  implicit none\n  type(string_t), allocatable :: src(:), dst(:)\n  allocate(src(1))\n  src(1)%str = 'echo hello'\n  dst = clone(src, 1)\n  if (.not. allocated(dst)) error stop 1\n  if (size(dst) /= 1) error stop 2\n  if (trim(dst(1)%str) /= 'echo hello') error stop 3\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("allocatable_derived_array_result_assign", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable derived array result compile spawn failed");
    assert!(
        compile.status.success(),
        "allocatable derived array result should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "allocatable derived array result should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected allocatable derived array result output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn hidden_result_call_accepts_component_char_array_element_actual() {
    let dir = unique_dir("hidden_result_component_char_actual");
    let types_src = write_program_in(
        &dir,
        "types.f90",
        "module types_mod\n  implicit none\n  type :: string_t\n    character(len=:), allocatable :: str\n  end type\n  type :: shell_function_t\n    character(len=256) :: name = ''\n    type(string_t), allocatable :: body(:)\n    integer :: body_lines = 0\n  end type\n  type :: shell_state_t\n    type(shell_function_t) :: functions(8)\n    integer :: num_functions = 0\n  end type\n  type :: command_t\n    character(len=:), allocatable :: tokens(:)\n    integer :: num_tokens = 0\n  end type\nend module types_mod\n",
    );
    let vars_src = write_program_in(
        &dir,
        "vars.f90",
        "module vars_mod\n  use types_mod\n  implicit none\ncontains\n  subroutine initialize_shell(shell)\n    type(shell_state_t), intent(out) :: shell\n    integer :: i\n    do i = 1, size(shell%functions)\n      shell%functions(i)%name = ''\n      shell%functions(i)%body_lines = 0\n    end do\n  end subroutine initialize_shell\n\n  subroutine add_function(shell, name, body_lines, body_count)\n    type(shell_state_t), intent(inout) :: shell\n    character(len=*), intent(in) :: name\n    character(len=*), intent(in) :: body_lines(:)\n    integer, intent(in) :: body_count\n    integer :: i, j\n    do i = 1, size(shell%functions)\n      if (trim(shell%functions(i)%name) == trim(name) .or. len_trim(shell%functions(i)%name) == 0) then\n        shell%functions(i)%name = name\n        shell%functions(i)%body_lines = body_count\n        if (allocated(shell%functions(i)%body)) deallocate(shell%functions(i)%body)\n        allocate(shell%functions(i)%body(body_count))\n        do j = 1, body_count\n          shell%functions(i)%body(j)%str = trim(body_lines(j))\n        end do\n        shell%num_functions = max(shell%num_functions, i)\n        return\n      end if\n    end do\n  end subroutine add_function\n\n  function get_function_body(shell, name) result(body)\n    type(shell_state_t), intent(in) :: shell\n    character(len=*), intent(in) :: name\n    type(string_t), allocatable :: body(:)\n    integer :: i, j\n    do i = 1, shell%num_functions\n      if (trim(shell%functions(i)%name) == trim(name)) then\n        if (allocated(shell%functions(i)%body)) then\n          allocate(body(shell%functions(i)%body_lines))\n          do j = 1, shell%functions(i)%body_lines\n            body(j)%str = shell%functions(i)%body(j)%str\n          end do\n        end if\n        return\n      end if\n    end do\n  end function get_function_body\nend module vars_mod\n",
    );
    let exec_src = write_program_in(
        &dir,
        "exec.f90",
        "module exec_mod\n  use types_mod\n  use vars_mod, only: get_function_body\n  implicit none\ncontains\n  subroutine run(shell, cmd)\n    type(shell_state_t), intent(in) :: shell\n    type(command_t), intent(in) :: cmd\n    type(string_t), allocatable :: body(:)\n    body = get_function_body(shell, cmd%tokens(1))\n    if (.not. allocated(body)) error stop 1\n    if (size(body) /= 1) error stop 2\n    if (.not. allocated(body(1)%str)) error stop 3\n    if (trim(body(1)%str) /= 'echo hello') error stop 4\n    print *, 'ok'\n  end subroutine run\nend module exec_mod\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use types_mod\n  use vars_mod, only: initialize_shell, add_function\n  use exec_mod, only: run\n  implicit none\n  type(shell_state_t) :: shell\n  type(command_t) :: cmd\n  call initialize_shell(shell)\n  call add_function(shell, 'myfunc', ['echo hello'], 1)\n  allocate(character(len=16) :: cmd%tokens(1))\n  cmd%tokens(1) = 'myfunc'\n  cmd%num_tokens = 1\n  call run(shell, cmd)\nend program p\n",
    );

    let types_obj = dir.join("types.o");
    let compile_types = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            types_src.to_str().unwrap(),
            "-o",
            types_obj.to_str().unwrap(),
        ])
        .output()
        .expect("types module compile spawn failed");
    assert!(
        compile_types.status.success(),
        "types module should compile: {}",
        String::from_utf8_lossy(&compile_types.stderr)
    );

    let vars_obj = dir.join("vars.o");
    let compile_vars = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            vars_src.to_str().unwrap(),
            "-o",
            vars_obj.to_str().unwrap(),
        ])
        .output()
        .expect("vars module compile spawn failed");
    assert!(
        compile_vars.status.success(),
        "vars module should compile: {}",
        String::from_utf8_lossy(&compile_vars.stderr)
    );

    let exec_obj = dir.join("exec.o");
    let compile_exec = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            exec_src.to_str().unwrap(),
            "-o",
            exec_obj.to_str().unwrap(),
        ])
        .output()
        .expect("exec module compile spawn failed");
    assert!(
        compile_exec.status.success(),
        "exec module should compile: {}",
        String::from_utf8_lossy(&compile_exec.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("main compile spawn failed");
    assert!(
        compile_main.status.success(),
        "main program should compile: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("hidden_result_component_char_actual.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            types_obj.to_str().unwrap(),
            vars_obj.to_str().unwrap(),
            exec_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "hidden-result component-char actual objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run spawn failed");
    assert!(
        run.status.success(),
        "hidden-result component-char actual binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected hidden-result component-char actual output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deferred_char_function_result_round_trips_across_amod_and_runs() {
    let dir = unique_dir("deferred_char_result_runtime");
    let mod_src = write_program_in(
        &dir,
        "builder.f90",
        "module builder\ncontains\n  function make_text(n) result(text)\n    integer, intent(in) :: n\n    integer :: i\n    character(len=:), allocatable :: text\n    allocate(character(len=n) :: text)\n    do i = 1, n\n      text(i:i) = 'x'\n    end do\n  end function make_text\nend module builder\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use builder\n  implicit none\n  character(len=:), allocatable :: s\n  s = make_text(3)\n  if (len(s) /= 3) error stop 1\n  if (s /= 'xxx') error stop 2\n  print *, trim(s)\nend program\n",
    );

    let mod_obj = dir.join("builder.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("builder module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "deferred-char builder module should compile: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("main compile spawn failed");
    assert!(
        compile_main.status.success(),
        "imported deferred-char result caller should compile: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("deferred_char_result.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            mod_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "deferred-char result objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run spawn failed");
    assert!(
        run.status.success(),
        "deferred-char result binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("xxx"),
        "unexpected deferred-char result output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn fixed_allocatable_character_substring_compiles_and_runs() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=16), allocatable :: buffer\n  allocate(buffer)\n  buffer = ''\n  buffer(1:1) = 'A'\n  if (buffer(1:1) /= 'A') error stop 1\n  print *, trim(buffer)\nend program\n",
        "f90",
    );
    let out = unique_path("alloc_char_substring", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable-char substring compile spawn failed");
    assert!(
        compile.status.success(),
        "fixed allocatable character substring should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "fixed allocatable character substring should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains('A'),
        "unexpected allocatable-char substring output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_scalar_substring_actual_preserves_hidden_len() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=:), allocatable :: buf\n  allocate(character(len=3) :: buf)\n  buf = 'ok '\n  call check(buf(1:2))\n  print *, 'ok'\ncontains\n  subroutine check(value)\n    character(len=*), intent(in) :: value\n    if (len(value) /= 2) error stop 1\n    if (value /= 'ok') error stop 2\n  end subroutine check\nend program\n",
        "f90",
    );
    let out = unique_path("alloc_scalar_substring_len_star", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable scalar substring compile spawn failed");
    assert!(
        compile.status.success(),
        "allocatable scalar substring should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "allocatable scalar substring should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected allocatable scalar substring output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn zero_length_allocatable_substring_in_and_chain_runs() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=:), allocatable :: assign_value\n  integer :: value_len\n  allocate(character(len=0) :: assign_value)\n  value_len = 0\n  if (value_len >= 2 .and. assign_value(1:1) == '(' .and. &\n      assign_value(value_len:value_len) == ')') then\n    print *, 'array'\n  end if\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("zero_len_alloc_substring_and", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("zero-length allocatable substring program compile spawn failed");
    assert!(
        compile.status.success(),
        "zero-length allocatable substring program should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "zero-length allocatable substring program should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected zero-length allocatable substring output: {}",
        stdout
    );
    assert!(
        !stdout.contains("array"),
        "zero-length allocatable substring should not satisfy guarded compare: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn local_array_element_does_not_fall_back_to_unrelated_char_symbol() {
    let src = write_program(
        "module helper_mod\ncontains\n  function values(i) result(out)\n    integer, intent(in) :: i\n    character(len=1) :: out\n    if (i > 0) then\n      out = 'x'\n    else\n      out = 'y'\n    end if\n  end function values\nend module helper_mod\n\nprogram p\n  implicit none\n  integer :: values(8)\n  values = 0\n  values(2) = 5\n  if (values(2) >= 1 .and. values(2) <= 12) print *, values(2)\nend program\n",
        "f90",
    );
    let out = unique_path("local_array_element_scope", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("local-array-element compile spawn failed");
    assert!(
        compile.status.success(),
        "local array element should not lower as an unrelated character call: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "local array element binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains('5'),
        "unexpected local-array-element output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn pointer_component_rhs_pointer_component_association_runs() {
    let src = write_program(
        "program p\n  implicit none\n  type :: node_t\n    integer :: node_type = 0\n    type(node_t), pointer :: body => null()\n  end type\n  type :: entry_t\n    type(node_t), pointer :: body => null()\n  end type\n  type(entry_t) :: cache\n  type(node_t), target :: root, leaf\n\n  leaf%node_type = 42\n  root%body => leaf\n  cache%body => root%body\n  if (.not. associated(cache%body)) error stop 1\n  if (cache%body%node_type /= 42) error stop 2\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("pointer_component_rhs_pointer_component", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("pointer component rhs pointer component compile spawn failed");
    assert!(
        compile.status.success(),
        "pointer component rhs pointer component should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "pointer component rhs pointer component should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected pointer component rhs pointer component output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn pointer_dummy_rhs_name_component_assignment_preserves_pointee() {
    let src = write_program(
        "program p\n  implicit none\n  type :: node_t\n    integer :: node_type = 0\n    type(list_t), pointer :: list => null()\n  end type\n  type :: list_t\n    type(node_t), pointer :: left => null()\n    type(node_t), pointer :: right => null()\n  end type\n  type(node_t), pointer :: a, b, root\n\n  allocate(a)\n  a%node_type = 11\n  allocate(b)\n  b%node_type = 22\n\n  root => create_list(a, b)\n\n  if (.not. associated(root%list%left)) error stop 1\n  if (.not. associated(root%list%right)) error stop 2\n  if (root%list%left%node_type /= 11) error stop 3\n  if (root%list%right%node_type /= 22) error stop 4\n  print *, 'ok'\ncontains\n  function create_list(left, right) result(node)\n    type(node_t), pointer, intent(in) :: left, right\n    type(node_t), pointer :: node\n    allocate(node)\n    node%node_type = 33\n    allocate(node%list)\n    node%list%left => left\n    node%list%right => right\n  end function create_list\nend program p\n",
        "f90",
    );
    let out = unique_path("pointer_dummy_rhs_name_component", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("pointer dummy rhs name component compile spawn failed");
    assert!(
        compile.status.success(),
        "pointer dummy rhs name component should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "pointer dummy rhs name component should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected pointer dummy rhs name component output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn pointer_array_component_element_actual_to_pointer_dummy_runs() {
    let src = write_program(
        "program p\n  implicit none\n  type :: node_t\n    integer :: node_type = 0\n    type(node_t), pointer :: child => null()\n  end type\n  type :: pipeline_t\n    type(node_t), pointer :: commands(:) => null()\n  end type\n  type :: wrapper_t\n    type(pipeline_t), pointer :: pipe => null()\n  end type\n  type(node_t), target :: storage(2)\n  type(node_t), pointer :: cmds(:)\n  type(wrapper_t) :: w\n\n  storage(1)%node_type = 11\n  storage(2)%node_type = 22\n  cmds => storage\n\n  allocate(w%pipe)\n  w%pipe%commands => cmds\n\n  call show_node(w%pipe%commands(1))\n  call show_node(w%pipe%commands(2))\ncontains\n  subroutine show_node(node)\n    type(node_t), pointer, intent(in) :: node\n    if (.not. associated(node)) error stop 1\n    print '(A,I0)', 'NODE=', node%node_type\n  end subroutine show_node\nend program p\n",
        "f90",
    );
    let out = unique_path("pointer_array_component_element_actual", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("pointer array component element actual compile spawn failed");
    assert!(
        compile.status.success(),
        "pointer array component element actual should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "pointer array component element actual should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("11") && stdout.contains("22"),
        "unexpected pointer array component element actual output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn single_char_array_constructor_actual_to_assumed_shape_dummy_runs() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=:), allocatable :: s\n  s = 'echo hello'\n  call show([s], 1)\ncontains\n  subroutine show(body_lines, body_count)\n    character(len=*), intent(in) :: body_lines(:)\n    integer, intent(in) :: body_count\n    if (size(body_lines) /= 1) error stop 1\n    if (body_count /= 1) error stop 2\n    if (trim(body_lines(1)) /= 'echo hello') error stop 3\n    print *, 'ok'\n  end subroutine show\nend program p\n",
        "f90",
    );
    let out = unique_path("char_array_constructor_actual", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("char array constructor actual compile spawn failed");
    assert!(
        compile.status.success(),
        "char array constructor actual should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "char array constructor actual should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected char array constructor actual output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn derived_pointer_module_global_survives_amod_import() {
    let dir = unique_dir("derived_ptr_amod");
    let mod_src = write_program_in(
        &dir,
        "state_mod.f90",
        "module state_mod\n  implicit none\n  type :: node_t\n    integer :: value = 0\n  end type node_t\n  type(node_t), target, save :: backing\n  type(node_t), pointer, public, save :: current => null()\ncontains\n  subroutine init_state()\n    current => backing\n    current%value = 1\n  end subroutine init_state\nend module state_mod\n",
    );
    let user_src = write_program_in(
        &dir,
        "user_mod.f90",
        "module user_mod\n  implicit none\ncontains\n  subroutine bump()\n    use state_mod\n    if (.not. associated(current)) call init_state()\n    current%value = current%value + 1\n  end subroutine bump\nend module user_mod\n",
    );

    let mod_obj = dir.join("state_mod.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("state module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "derived-pointer module should compile: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let user_obj = dir.join("user_mod.o");
    let compile_user = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            user_src.to_str().unwrap(),
            "-o",
            user_obj.to_str().unwrap(),
        ])
        .output()
        .expect("state user compile spawn failed");
    assert!(
        compile_user.status.success(),
        "imported derived-pointer module globals should survive .amod export/import: {}",
        String::from_utf8_lossy(&compile_user.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deferred_char_pointer_component_compiles_string_pool_style_ops() {
    let src = write_program(
        "module m\n  implicit none\n  type :: string_ref\n    integer :: str_len = 0\n    character(:), pointer :: data => null()\n  end type string_ref\n  character(len=16), target :: pool(1)\ncontains\n  subroutine bind_pool(ref, n)\n    type(string_ref), intent(inout) :: ref\n    integer, intent(in) :: n\n    ref%str_len = n\n    ref%data => pool(1)(1:n)\n    if (associated(ref%data)) then\n      ref%data = ' '\n      ref%data(1:1) = 'x'\n    end if\n  end subroutine bind_pool\n\n  subroutine own_alloc(ref, n)\n    type(string_ref), intent(inout) :: ref\n    integer, intent(in) :: n\n    if (associated(ref%data)) deallocate(ref%data)\n    allocate(character(len=n) :: ref%data)\n    ref%data = 'abc'\n  end subroutine own_alloc\nend module\n",
        "f90",
    );
    let out = unique_path("deferred_char_pointer_component", "o");
    let result = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "deferred char pointer components should compile: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn deferred_char_pointer_component_can_bind_allocatable_char_array_element() {
    let src = write_program(
        "program p\n  implicit none\n  type :: string_ref\n    character(:), pointer :: data => null()\n  end type string_ref\n  type(string_ref) :: ref\n  character(len=32), target, allocatable :: pool(:)\n\n  allocate(pool(1))\n  pool = ''\n  ref%data => pool(1)(1:32)\n  if (.not. associated(ref%data)) error stop 1\n  ref%data = '/tmp'\n  if (trim(ref%data) /= '/tmp') error stop 2\n  print *, trim(ref%data)\nend program\n",
        "f90",
    );
    let out = unique_path("deferred_char_ptr_alloc_char_elem", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable char element pointer bind compile spawn failed");
    assert!(
        compile.status.success(),
        "allocatable char element pointer bind should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "allocatable char element pointer bind should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("/tmp"),
        "unexpected allocatable char element pointer bind output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn get_environment_variable_literal_name_populates_value_and_status() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=16) :: home_buf\n  integer :: len_out, stat_out\n  home_buf = ''\n  len_out = -1\n  stat_out = -1\n  call get_environment_variable('HOME', home_buf, len_out, stat_out)\n  if (stat_out /= 0) error stop 1\n  if (len_out /= 4) error stop 2\n  if (trim(home_buf) /= '/tmp') error stop 3\n  print *, trim(home_buf)\nend program\n",
        "f90",
    );
    let out = unique_path("get_environment_variable_literal_name", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("literal-name get_environment_variable compile spawn failed");
    assert!(
        compile.status.success(),
        "literal-name get_environment_variable should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .env("HOME", "/tmp")
        .output()
        .expect("run failed");
    assert!(
        run.status.success(),
        "literal-name get_environment_variable should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("/tmp"),
        "unexpected literal-name get_environment_variable output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn logical_allocatable_slice_assignment_compiles() {
    let src = write_program(
        "program p\n  implicit none\n  logical, allocatable :: a(:), b(:)\n  integer :: n\n  n = 4\n  allocate(a(n), b(n))\n  a = .false.\n  b = .true.\n  a(1:n) = b(1:n)\n  b(2:n-1) = .false.\nend program\n",
        "f90",
    );
    let out = unique_path("logical_slice_assign", "o");
    let result = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "logical allocatable slice assignment should compile: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn c_interop_opaque_pointer_values_compile() {
    let src = write_program(
        "program p\n  use iso_c_binding\n  implicit none\n  type(c_ptr) :: pbuf\n  type(c_funptr) :: fptr\n  pbuf = c_null_ptr\n  fptr = c_null_funptr\nend program\n",
        "f90",
    );
    let out = unique_path("c_interop_opaque_values", "o");
    let result = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "C interop opaque pointer values should compile: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocated_eqv_on_allocatables_compiles() {
    let src = write_program(
        "program p\n  implicit none\n  logical, allocatable :: a(:), b(:)\n  logical :: same\n  allocate(a(1), b(1))\n  same = allocated(a) .eqv. allocated(b)\nend program\n",
        "f90",
    );
    let out = unique_path("allocated_eqv", "o");
    let result = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "allocated() logical combinations should compile: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn runtime_bound_local_c_ptr_array_accepts_final_null_slot() {
    let src = write_program(
        "program p\n  use iso_c_binding, only: c_ptr, c_null_ptr, c_associated\n  implicit none\n  call run(1)\ncontains\n  subroutine run(n)\n    integer, intent(in) :: n\n    type(c_ptr), target :: argv(n + 1)\n    argv(n + 1) = c_null_ptr\n    if (c_associated(argv(n + 1))) error stop 1\n    print *, 'ok'\n  end subroutine run\nend program\n",
        "f90",
    );
    let out = unique_path("runtime_bound_c_ptr_auto_array", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("runtime-bound c_ptr array compile failed to spawn");
    assert!(
        compile.status.success(),
        "runtime-bound c_ptr array compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("runtime-bound c_ptr array run failed");
    assert!(
        run.status.success(),
        "runtime-bound c_ptr array run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected runtime-bound c_ptr array output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn whole_component_char_array_actual_preserves_hidden_len() {
    let src = write_program(
        "program p\n  implicit none\n  type :: command_t\n    character(len=:), allocatable :: tokens(:)\n  end type command_t\n  type(command_t) :: cmd\n  allocate(character(len=8) :: cmd%tokens(1))\n  cmd%tokens(1) = 'true'\n  call check(cmd%tokens)\ncontains\n  subroutine check(tokens)\n    character(len=*), intent(in) :: tokens(:)\n    if (len(tokens(1)) /= 8) error stop 1\n    if (trim(tokens(1)) /= 'true') error stop 2\n    print *, trim(tokens(1))\n  end subroutine check\nend program\n",
        "f90",
    );
    let out = unique_path("component_char_array_actual_hidden_len", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("component char array actual compile failed to spawn");
    assert!(
        compile.status.success(),
        "component char array actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("component char array actual run failed");
    assert!(
        run.status.success(),
        "component char array actual run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("true"),
        "unexpected component char array actual output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn deferred_char_component_array_copy_preserves_contents() {
    let src = write_program(
        "program p\n  implicit none\n  type :: command_t\n    character(len=:), allocatable :: tokens(:)\n  end type command_t\n  type(command_t) :: src_cmd, dst_cmd\n  allocate(character(len=4) :: src_cmd%tokens(2), dst_cmd%tokens(2))\n  src_cmd%tokens(1) = 'read'\n  src_cmd%tokens(2) = 'line'\n  dst_cmd%tokens = src_cmd%tokens\n  if (trim(dst_cmd%tokens(1)) /= 'read') error stop 1\n  if (trim(dst_cmd%tokens(2)) /= 'line') error stop 2\n  print *, trim(dst_cmd%tokens(1)), trim(dst_cmd%tokens(2))\nend program\n",
        "f90",
    );
    let out = unique_path("component_char_array_copy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("component char array copy compile failed to spawn");
    assert!(
        compile.status.success(),
        "component char array copy compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("component char array copy run failed");
    assert!(
        run.status.success(),
        "component char array copy run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("read") && stdout.contains("line"),
        "unexpected component char array copy output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn derived_scalar_assignment_deep_copies_allocatable_char_component() {
    let src = write_program(
        "program p\n  implicit none\n  type :: redirect_t\n    integer :: kind = 0\n    integer :: fd = -1\n    integer :: target_fd = -1\n    character(len=:), allocatable :: filename\n    logical :: force_clobber = .false.\n  end type redirect_t\n  type(redirect_t) :: src_redir, dst_redir\n  src_redir%kind = 7\n  src_redir%fd = 0\n  src_redir%target_fd = -1\n  allocate(src_redir%filename, source='alpha')\n  src_redir%force_clobber = .true.\n  dst_redir = src_redir\n  src_redir%filename = 'omega'\n  if (.not. allocated(dst_redir%filename)) error stop 1\n  if (trim(dst_redir%filename) /= 'alpha') error stop 2\n  if (dst_redir%kind /= 7) error stop 3\n  if (dst_redir%fd /= 0) error stop 4\n  if (dst_redir%target_fd /= -1) error stop 5\n  if (.not. dst_redir%force_clobber) error stop 6\n  if (trim(src_redir%filename) /= 'omega') error stop 7\n  print *, trim(dst_redir%filename)\nend program\n",
        "f90",
    );
    let out = unique_path("derived_scalar_alloc_char_copy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived scalar alloc-char copy compile failed to spawn");
    assert!(
        compile.status.success(),
        "derived scalar alloc-char copy compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("derived scalar alloc-char copy run failed");
    assert!(
        run.status.success(),
        "derived scalar alloc-char copy run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("alpha"),
        "unexpected derived scalar alloc-char copy output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn derived_section_assignment_deep_copies_allocatable_char_component() {
    let src = write_program(
        "program p\n  implicit none\n  type :: redirect_t\n    integer :: kind = 0\n    integer :: fd = -1\n    integer :: target_fd = -1\n    character(len=:), allocatable :: filename\n    logical :: force_clobber = .false.\n  end type redirect_t\n  type(redirect_t), allocatable :: src_redirs(:)\n  type(redirect_t) :: dst_redirs(1)\n  allocate(src_redirs(1))\n  src_redirs(1)%kind = 7\n  src_redirs(1)%fd = 0\n  src_redirs(1)%target_fd = -1\n  allocate(src_redirs(1)%filename, source='alpha')\n  src_redirs(1)%force_clobber = .true.\n  dst_redirs(1:1) = src_redirs(1:1)\n  src_redirs(1)%filename = 'omega'\n  if (.not. allocated(dst_redirs(1)%filename)) error stop 1\n  if (trim(dst_redirs(1)%filename) /= 'alpha') error stop 2\n  if (dst_redirs(1)%kind /= 7) error stop 3\n  if (dst_redirs(1)%fd /= 0) error stop 4\n  if (dst_redirs(1)%target_fd /= -1) error stop 5\n  if (.not. dst_redirs(1)%force_clobber) error stop 6\n  if (trim(src_redirs(1)%filename) /= 'omega') error stop 7\n  print *, trim(dst_redirs(1)%filename)\nend program\n",
        "f90",
    );
    let out = unique_path("derived_section_alloc_char_copy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived section alloc-char copy compile failed to spawn");
    assert!(
        compile.status.success(),
        "derived section alloc-char copy compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("derived section alloc-char copy run failed");
    assert!(
        run.status.success(),
        "derived section alloc-char copy run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("alpha"),
        "unexpected derived section alloc-char copy output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn empty_allocatable_char_component_copy_stays_allocated() {
    let src = write_program(
        "program p\n  implicit none\n  type :: redirect_t\n    character(len=:), allocatable :: filename\n  end type redirect_t\n  type(redirect_t), allocatable :: src_redirs(:)\n  type(redirect_t) :: dst_redirs(1)\n  allocate(src_redirs(1))\n  src_redirs(1)%filename = ''\n  dst_redirs(1:1) = src_redirs(1:1)\n  if (.not. allocated(src_redirs(1)%filename)) error stop 1\n  if (.not. allocated(dst_redirs(1)%filename)) error stop 2\n  if (len(src_redirs(1)%filename) /= 0) error stop 3\n  if (len(dst_redirs(1)%filename) /= 0) error stop 4\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("empty_alloc_char_component_copy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("empty alloc-char component copy compile failed to spawn");
    assert!(
        compile.status.success(),
        "empty alloc-char component copy compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("empty alloc-char component copy run failed");
    assert!(
        run.status.success(),
        "empty alloc-char component copy run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected empty alloc-char component copy output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn module_global_derived_array_char_default_init_uses_blanks() {
    let src = write_program(
        "module m\n  implicit none\n  type :: entry_t\n    character(len=8) :: name = ''\n  end type\n  type(entry_t), save :: table(2)\ncontains\n  subroutine check()\n    if (len_trim(table(1)%name) /= 0) error stop 1\n    if (table(1)%name(1:1) /= ' ') error stop 2\n    print *, 'ok'\n  end subroutine\nend module\nprogram p\n  use m\n  call check()\nend program\n",
        "f90",
    );
    let out = unique_path("module_global_derived_array_blank_init", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("module global derived array blank-init compile failed to spawn");
    assert!(
        compile.status.success(),
        "module global derived array blank-init compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("module global derived array blank-init run failed");
    assert!(
        run.status.success(),
        "module global derived array blank-init run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected module global derived array blank-init output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn internal_read_from_char_array_element_uses_internal_file_path() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=16) :: words(2)\n  integer :: ios, fd\n  words(1) = '2'\n  ios = -99\n  fd = -1\n  read(words(1), *, iostat=ios) fd\n  if (ios /= 0) error stop 1\n  if (fd /= 2) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("internal_read_array_elem", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("internal read array-element compile failed to spawn");
    assert!(
        compile.status.success(),
        "internal read array-element compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("internal read array-element run failed");
    assert!(
        run.status.success(),
        "internal read array-element run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected internal read array-element output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn formatted_internal_read_from_char_component_uses_internal_file_path() {
    let src = write_program(
        "program p\n  implicit none\n  type :: token_t\n    character(len=16) :: value = ''\n  end type\n  type(token_t) :: tok\n  integer :: ios, fd\n  tok%value(1:1) = '3'\n  ios = -99\n  fd = -1\n  read(tok%value, '(I1)', iostat=ios) fd\n  if (ios /= 0) error stop 1\n  if (fd /= 3) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("formatted_internal_read_component", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("formatted internal read component compile failed to spawn");
    assert!(
        compile.status.success(),
        "formatted internal read component compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("formatted internal read component run failed");
    assert!(
        run.status.success(),
        "formatted internal read component run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected formatted internal read component output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn formatted_octal_internal_io_round_trips_min_digits_and_values() {
    let src = write_program(
        "program p\n  use iso_c_binding\n  implicit none\n  integer(c_int) :: current_mask, new_mask\n  integer :: ios\n  character(len=16) :: mask_str\n  current_mask = int(o'0022', c_int)\n  write(mask_str, '(o4.4)') current_mask\n  if (trim(adjustl(mask_str)) /= '0022') error stop 1\n  mask_str = '077'\n  read(mask_str, '(o10)', iostat=ios) new_mask\n  if (ios /= 0 .or. new_mask /= 63_c_int) error stop 2\n  mask_str = '22'\n  read(mask_str, '(o10)', iostat=ios) new_mask\n  if (ios /= 0 .or. new_mask /= 18_c_int) error stop 3\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("formatted_octal_internal_io", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("formatted octal internal io compile failed to spawn");
    assert!(
        compile.status.success(),
        "formatted octal internal io compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("formatted octal internal io run failed");
    assert!(
        run.status.success(),
        "formatted octal internal io run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected formatted octal internal io output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn derived_assignment_deep_copies_allocatable_command_tokens() {
    let src = write_program(
        "module repro\n  implicit none\n  type :: shell_state_t\n    integer :: last_exit_status = -99\n  end type\n  type :: command_t\n    character(len=:), allocatable :: tokens(:)\n    integer :: num_tokens = 0\n    integer, allocatable :: token_lengths(:)\n    logical, allocatable :: token_quoted(:)\n    logical, allocatable :: token_escaped(:)\n    integer, allocatable :: token_quote_type(:)\n  end type\ncontains\n  recursive subroutine execute_test_command(cmd, shell)\n    type(command_t), intent(in) :: cmd\n    type(shell_state_t), intent(inout) :: shell\n    logical :: test_result\n    logical :: left_result, right_result\n    type(command_t) :: sub_cmd, left_cmd, right_cmd\n    integer :: i, j, logical_op_pos\n    integer :: paren_depth, check_pos\n    logical :: outer_parens_wrap_all\n    integer :: effective_num_tokens\n    logical :: is_bracket_cmd\n    character(len=16) :: op\n\n    if (cmd%num_tokens < 2) then\n      shell%last_exit_status = 1\n      return\n    end if\n\n    is_bracket_cmd = (trim(cmd%tokens(1)) == '[')\n    if (is_bracket_cmd) then\n      effective_num_tokens = cmd%num_tokens - 1\n    else\n      effective_num_tokens = cmd%num_tokens\n    end if\n\n    if (effective_num_tokens == 4) then\n      op = cmd%tokens(3)\n      select case (trim(op))\n      case ('-gt')\n        test_result = string_to_int(cmd%tokens(2)) > string_to_int(cmd%tokens(4))\n      case default\n        test_result = .false.\n      end select\n    else if (effective_num_tokens >= 5) then\n      if (trim(cmd%tokens(2)) == '(') then\n        paren_depth = 1\n        outer_parens_wrap_all = .false.\n        do check_pos = 3, effective_num_tokens\n          if (trim(cmd%tokens(check_pos)) == '(') then\n            paren_depth = paren_depth + 1\n          else if (trim(cmd%tokens(check_pos)) == ')') then\n            paren_depth = paren_depth - 1\n            if (paren_depth == 0) then\n              outer_parens_wrap_all = (check_pos == effective_num_tokens)\n              exit\n            end if\n          end if\n        end do\n        if (outer_parens_wrap_all) then\n          sub_cmd = cmd\n          sub_cmd%tokens(1) = cmd%tokens(1)\n          if (is_bracket_cmd) then\n            sub_cmd%num_tokens = cmd%num_tokens - 2\n            do i = 2, sub_cmd%num_tokens - 1\n              sub_cmd%tokens(i) = cmd%tokens(i + 1)\n            end do\n            sub_cmd%tokens(sub_cmd%num_tokens) = ']'\n          else\n            sub_cmd%num_tokens = cmd%num_tokens - 2\n            do i = 2, sub_cmd%num_tokens\n              sub_cmd%tokens(i) = cmd%tokens(i + 1)\n            end do\n          end if\n          call execute_test_command(sub_cmd, shell)\n          return\n        end if\n      end if\n\n      logical_op_pos = 0\n      paren_depth = 0\n      do i = 2, effective_num_tokens\n        if (trim(cmd%tokens(i)) == '(') then\n          paren_depth = paren_depth + 1\n        else if (trim(cmd%tokens(i)) == ')') then\n          paren_depth = paren_depth - 1\n        else if (paren_depth == 0) then\n          if (trim(cmd%tokens(i)) == '-o') then\n            logical_op_pos = i\n            exit\n          else if (trim(cmd%tokens(i)) == '-a') then\n            if (logical_op_pos == 0) logical_op_pos = i\n          end if\n        end if\n      end do\n\n      if (logical_op_pos > 0) then\n        left_cmd = cmd\n        left_cmd%tokens(1) = 'test'\n        left_cmd%num_tokens = logical_op_pos - 1\n        do j = 2, left_cmd%num_tokens\n          left_cmd%tokens(j) = cmd%tokens(j)\n        end do\n\n        right_cmd = cmd\n        right_cmd%tokens(1) = 'test'\n        right_cmd%num_tokens = effective_num_tokens + 1 - logical_op_pos\n        do j = 2, right_cmd%num_tokens\n          right_cmd%tokens(j) = cmd%tokens(j + logical_op_pos - 1)\n        end do\n\n        call execute_test_command(left_cmd, shell)\n        left_result = (shell%last_exit_status == 0)\n        call execute_test_command(right_cmd, shell)\n        right_result = (shell%last_exit_status == 0)\n\n        if (trim(cmd%tokens(logical_op_pos)) == '-a') then\n          test_result = left_result .and. right_result\n        else\n          test_result = left_result .or. right_result\n        end if\n      else\n        test_result = .false.\n      end if\n    else\n      test_result = .false.\n    end if\n\n    if (test_result) then\n      shell%last_exit_status = 0\n    else\n      shell%last_exit_status = 1\n    end if\n  end subroutine\n\n  integer function string_to_int(str) result(v)\n    character(len=*), intent(in) :: str\n    integer :: ios\n    read(str, *, iostat=ios) v\n    if (ios /= 0) v = 0\n  end function\nend module\nprogram p\n  use repro\n  implicit none\n  type(command_t) :: cmd\n  type(shell_state_t) :: shell\n\n  allocate(character(len=16) :: cmd%tokens(12))\n  allocate(cmd%token_lengths(12), cmd%token_quoted(12), cmd%token_escaped(12), cmd%token_quote_type(12))\n  cmd%num_tokens = 12\n  cmd%tokens = ''\n  cmd%token_lengths = 0\n  cmd%token_quoted = .false.\n  cmd%token_escaped = .false.\n  cmd%token_quote_type = 0\n  cmd%tokens(1) = 'test'\n  cmd%tokens(2) = '('\n  cmd%tokens(3) = '5'\n  cmd%tokens(4) = '-gt'\n  cmd%tokens(5) = '3'\n  cmd%tokens(6) = ')'\n  cmd%tokens(7) = '-a'\n  cmd%tokens(8) = '('\n  cmd%tokens(9) = '10'\n  cmd%tokens(10) = '-gt'\n  cmd%tokens(11) = '8'\n  cmd%tokens(12) = ')'\n\n  call execute_test_command(cmd, shell)\n  if (shell%last_exit_status /= 0) error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("derived_assign_alloc_char_tokens", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived alloc-char token compile failed to spawn");
    assert!(
        compile.status.success(),
        "derived alloc-char token compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("derived alloc-char token run failed");
    assert!(
        run.status.success(),
        "derived alloc-char token run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected derived alloc-char token output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fixed_char_component_substring_assignment_updates_field() {
    let src = write_program(
        "program p\n  implicit none\n  type :: token_t\n    character(len=16) :: value = ''\n  end type\n  type(token_t) :: tok\n  tok%value = ''\n  tok%value(1:1) = '3'\n  if (tok%value(1:1) /= '3') error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("fixed_char_component_substring", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("fixed char component substring compile failed to spawn");
    assert!(
        compile.status.success(),
        "fixed char component substring compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("fixed char component substring run failed");
    assert!(
        run.status.success(),
        "fixed char component substring run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected fixed char component substring output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn fixed_char_component_substring_prints_contents() {
    let src = write_program(
        "program p\n  implicit none\n  type :: token_t\n    character(len=16) :: value = ''\n  end type\n  type(token_t) :: tok\n  tok%value = 'echo'\n  print *, tok%value(1:4)\nend program p\n",
        "f90",
    );
    let out = unique_path("fixed_char_component_print", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("fixed char component print compile failed to spawn");
    assert!(
        compile.status.success(),
        "fixed char component print compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("fixed char component print run failed");
    assert!(
        run.status.success(),
        "fixed char component print run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("echo"),
        "unexpected fixed char component print output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn logical_whole_array_copy_preserves_elements() {
    let src = write_program(
        "program p\n  implicit none\n  logical :: src(3), dest(3)\n  src = .false.\n  src(3) = .true.\n  dest = src\n  if (dest(1)) error stop 1\n  if (dest(2)) error stop 2\n  if (.not. dest(3)) error stop 3\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("logical_whole_array_copy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("logical whole-array copy compile failed to spawn");
    assert!(
        compile.status.success(),
        "logical whole-array copy compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("logical whole-array copy run failed");
    assert!(
        run.status.success(),
        "logical whole-array copy run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected logical whole-array copy output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn logical_section_copy_preserves_elements() {
    let src = write_program(
        "program p\n  implicit none\n  logical :: src(3), dest(3)\n  src = .false.\n  src(3) = .true.\n  dest = .false.\n  dest(1:3) = src(1:3)\n  if (dest(1)) error stop 1\n  if (dest(2)) error stop 2\n  if (.not. dest(3)) error stop 3\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("logical_section_copy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("logical section copy compile failed to spawn");
    assert!(
        compile.status.success(),
        "logical section copy compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("logical section copy run failed");
    assert!(
        run.status.success(),
        "logical section copy run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected logical section copy output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}
