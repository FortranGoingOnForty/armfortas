//! Sprint 32 CLI driver tests.
//!
//! Each test exercises one user-visible behaviour of the `armfortas`
//! / `afs` driver via subprocess invocation.  Subprocess use is
//! deliberate — we want to catch wrong-exit-code, wrong-stdout-vs-
//! stderr-routing, and missing-symbol-from-bin issues that an
//! in-process API call wouldn't see.

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
