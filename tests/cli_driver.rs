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
fn ambiguous_use_warning_is_deduped_across_contained_procedures() {
    let src = write_program(
        r#"
module mod_a
  implicit none
  integer :: x = 1
end module

module mod_b
  implicit none
  integer :: x = 2
end module

program demo
  use mod_a
  use mod_b
  implicit none
  print *, x
contains
  subroutine s1()
    print *, x
  end subroutine

  subroutine s2()
    print *, x
  end subroutine
end program
"#,
        "f90",
    );
    let out = unique_path("ambig_use", "o");
    let result = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("compile failed to spawn");
    assert!(
        result.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    let count = stderr
        .matches(
            "warning: ambiguous USE import 'x' from both 'mod_a' and 'mod_b'; keeping the first",
        )
        .count();
    assert_eq!(
        count, 1,
        "expected one deduped ambiguous-USE warning, got {}:\n{}",
        count, stderr
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn only_filtered_name_stays_accessible_when_imported_from_another_module() {
    let src = write_program(
        r#"
module mod_a
  implicit none
  integer :: current_search_pattern = 1
end module

module mod_b
  implicit none
  integer :: current_search_pattern = 2
  integer :: helper = 3
end module

program demo
  use mod_a, only: current_search_pattern
  use mod_b, only: helper
  implicit none
  print *, current_search_pattern + helper
end program
"#,
        "f90",
    );
    let out = unique_path("use_only_visible_other_module", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("compile failed to spawn");
    assert!(
        result.status.success(),
        "name imported from one module should remain accessible even if filtered from another: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
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
fn formatted_char_pointer_read_preserves_blank_record_before_following_input() {
    let src = write_program(
        "program p\n  implicit none\n  character(:), pointer :: line\n  integer :: ios\n  allocate(character(len=32) :: line)\n  line = 'seed'\n  read(*,'(a)',iostat=ios) line\n  write(*,'(a,i0,a,a,a)') 'IOS1=', ios, ' LINE1=<', trim(line), '>'\n  read(*,'(a)',iostat=ios) line\n  write(*,'(a,i0,a,a,a)') 'IOS2=', ios, ' LINE2=<', trim(line), '>'\n  read(*,'(a)',iostat=ios) line\n  write(*,'(a,i0)') 'IOS3=', ios\nend program\n",
        "f90",
    );
    let out = unique_path("formatted_char_pointer_blank_record", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("formatted char pointer blank-record compile failed to spawn");
    assert!(
        compile.status.success(),
        "formatted char pointer blank-record compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let input = unique_path("formatted_char_pointer_blank_record_input", "txt");
    std::fs::write(&input, "\nhello\n")
        .expect("cannot write formatted char pointer blank-record input");
    let run = Command::new(&out)
        .stdin(std::fs::File::open(&input).expect("cannot open formatted char pointer input"))
        .output()
        .expect("formatted char pointer blank-record run failed");
    assert!(
        run.status.success(),
        "formatted char pointer blank-record run failed: {:?}\nstderr: {}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("IOS1=0 LINE1=<>"),
        "expected blank first record to succeed as empty string, got: {}",
        stdout
    );
    assert!(
        stdout.contains("IOS2=0 LINE2=<hello>"),
        "expected second record to stay readable after blank line, got: {}",
        stdout
    );
    assert!(
        stdout.contains("IOS3=-1"),
        "expected EOF after the second record, got: {}",
        stdout
    );

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn stream_unformatted_scalar_char_read_preserves_each_byte() {
    let input = unique_path("stream_unformatted_char_read", "bin");
    std::fs::write(&input, b"A\0B").expect("cannot write stream char read input");

    let src = write_program(
        &format!(
            "program p\n  implicit none\n  integer :: unit_num, ios\n  character(len=1) :: ch\n  open(newunit=unit_num, file='{}', status='old', action='read', access='stream', form='unformatted', iostat=ios)\n  if (ios /= 0) error stop 1\n  do\n    read(unit_num, iostat=ios) ch\n    if (ios /= 0) exit\n    print '(i0)', iachar(ch)\n  end do\n  print '(a,i0)', 'IOS=', ios\n  close(unit_num)\nend program\n",
            input.display()
        ),
        "stream_unformatted_char_read.f90",
    );
    let out = unique_path("stream_unformatted_char_read", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("stream unformatted char read compile failed to spawn");
    assert!(
        compile.status.success(),
        "stream unformatted char read compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("stream unformatted char read run failed");
    assert!(
        run.status.success(),
        "stream unformatted char read run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert_eq!(
        stdout
            .lines()
            .map(|line| line
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>())
            .collect::<Vec<_>>(),
        vec!["65", "0", "66", "IOS=-1"],
        "unexpected stream unformatted char read output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&input);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn stream_unformatted_char_write_preserves_exact_bytes() {
    let output_file = unique_path("stream_unformatted_char_write", "bin");
    let src = write_program(
        &format!(
            "program p\n  implicit none\n  integer :: unit_num, ios\n  open(newunit=unit_num, file='{}', status='replace', action='write', access='stream', form='unformatted', iostat=ios)\n  if (ios /= 0) error stop 1\n  write(unit_num, iostat=ios) 'alpha'\n  if (ios /= 0) error stop 2\n  close(unit_num)\nend program\n",
            output_file.display()
        ),
        "stream_unformatted_char_write.f90",
    );
    let out = unique_path("stream_unformatted_char_write", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("stream unformatted char write compile failed to spawn");
    assert!(
        compile.status.success(),
        "stream unformatted char write compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("stream unformatted char write run failed");
    assert!(
        run.status.success(),
        "stream unformatted char write run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let bytes = std::fs::read(&output_file).expect("cannot read stream unformatted char output");
    assert_eq!(
        bytes, b"alpha",
        "expected exact bytes from stream-unformatted character write, got {:?}",
        bytes
    );

    let _ = std::fs::remove_file(&output_file);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn repeated_nonadvancing_a1_read_preserves_embedded_nul_bytes() {
    let input = unique_path("nonadvancing_a1_char_read", "bin");
    std::fs::write(&input, b"A\0B").expect("cannot write nonadvancing A1 input");

    let src = write_program(
        "program p\n  implicit none\n  integer :: ios\n  character(len=1) :: ch\n  do\n    read(*, '(A1)', advance='no', iostat=ios) ch\n    if (ios /= 0) exit\n    print '(i0)', iachar(ch)\n  end do\n  print '(a,i0)', 'IOS=', ios\nend program\n",
        "nonadvancing_a1_char_read.f90",
    );
    let out = unique_path("nonadvancing_a1_char_read", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("nonadvancing A1 char read compile failed to spawn");
    assert!(
        compile.status.success(),
        "nonadvancing A1 char read compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .stdin(std::fs::File::open(&input).expect("cannot open nonadvancing A1 input"))
        .output()
        .expect("nonadvancing A1 char read run failed");
    assert!(
        run.status.success(),
        "nonadvancing A1 char read run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert_eq!(
        stdout
            .lines()
            .map(|line| line
                .chars()
                .filter(|ch| !ch.is_whitespace())
                .collect::<String>())
            .collect::<Vec<_>>(),
        vec!["65", "0", "66", "IOS=-2"],
        "unexpected nonadvancing A1 char read output: {}",
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
fn bind_c_exit_flushes_pending_nonadvancing_stdout_output() {
    let src = write_program(
        "program p\n  use iso_c_binding, only: c_int\n  implicit none\n  interface\n    subroutine c_exit(code) bind(C, name='exit')\n      import :: c_int\n      integer(c_int), value :: code\n    end subroutine\n  end interface\n  write(*, '(A,A)', advance='no') 'hello', char(0)\n  call c_exit(0_c_int)\nend program\n",
        "f90",
    );
    let out = unique_path("bind_c_exit_flushes_pending_stdout", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("bind(c) exit flush compile failed to spawn");
    assert!(
        compile.status.success(),
        "bind(c) exit flush compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("bind(c) exit flush run failed");
    assert!(
        run.status.success(),
        "bind(c) exit flush run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        run.stdout, b"hello\0",
        "bind(c) exit should flush pending non-advancing stdout writes"
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn call_flush_intrinsic_subroutine_links_and_runs() {
    let src = write_program(
        "program p\n  use iso_fortran_env, only: output_unit\n  implicit none\n  write(output_unit, '(A)', advance='no') 'ok'\n  call flush(output_unit)\nend program\n",
        "f90",
    );
    let out = unique_path("call_flush_intrinsic_subroutine", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("call flush intrinsic compile failed to spawn");
    assert!(
        compile.status.success(),
        "call flush intrinsic compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("call flush intrinsic run failed");
    assert!(
        run.status.success(),
        "call flush intrinsic run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(run.stdout, b"ok");

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn real_mod_intrinsic_compiles_and_runs() {
    let src = write_program(
        "program p\n  use iso_fortran_env, only: real64\n  implicit none\n  real(real64) :: x\n  x = mod(-5.5_real64, 2.0_real64)\n  if (abs(x + 1.5_real64) > 1.0e-12_real64) error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("real_mod_intrinsic", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-O2", "-o", out.to_str().unwrap()])
        .output()
        .expect("real mod compile failed to spawn");
    assert!(
        compile.status.success(),
        "real mod compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("real mod run failed");
    assert!(
        run.status.success(),
        "real mod run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn reshape_array_actual_to_assumed_shape_dummy_compiles_and_runs() {
    let src = write_program(
        "module m\ncontains\n  function wrap(matrix_data) result(total)\n    use iso_fortran_env, only: real64\n    real(real64), intent(in) :: matrix_data(:,:)\n    real(real64) :: total\n    if (size(matrix_data, 1) /= 2 .or. size(matrix_data, 2) /= 2) error stop 11\n    total = matrix_data(2, 1) + matrix_data(1, 2)\n  end function wrap\nend module m\n\nprogram p\n  use m\n  use iso_fortran_env, only: real64\n  implicit none\n  real(real64) :: total\n  total = wrap(reshape([1.0_real64, 3.0_real64, 2.0_real64, 4.0_real64], [2, 2]))\n  if (abs(total - 5.0_real64) > 1.0e-12_real64) error stop 12\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("reshape_array_actual", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-O2", "-o", out.to_str().unwrap()])
        .output()
        .expect("reshape actual compile failed to spawn");
    assert!(
        compile.status.success(),
        "reshape actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("reshape actual run failed");
    assert!(
        run.status.success(),
        "reshape actual run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected reshape actual output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn function_result_kind_alias_flows_to_caller_abi() {
    let src = write_program(
        "module m\ncontains\n  function wrap() result(total)\n    use iso_fortran_env, only: real64\n    real(real64) :: total\n    total = 5.0_real64\n  end function wrap\nend module m\n\nprogram p\n  use m\n  use iso_fortran_env, only: real64\n  implicit none\n  real(real64) :: total\n  total = wrap()\n  if (abs(total - 5.0_real64) > 1.0e-12_real64) error stop 1\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("function_result_kind_alias", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-O2", "-o", out.to_str().unwrap()])
        .output()
        .expect("function result kind alias compile failed to spawn");
    assert!(
        compile.status.success(),
        "function result kind alias compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("function result kind alias run failed");
    assert!(
        run.status.success(),
        "function result kind alias run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected function result kind alias output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn reshape_array_actual_to_assumed_shape_subroutine_dummy_compiles_and_runs() {
    let src = write_program(
        "module m\ncontains\n  subroutine show(matrix_data)\n    use iso_fortran_env, only: real64\n    real(real64), intent(in) :: matrix_data(:,:)\n    if (size(matrix_data, 1) /= 2 .or. size(matrix_data, 2) /= 2) error stop 21\n    if (abs(matrix_data(2, 1) - 3.0_real64) > 1.0e-12_real64) error stop 22\n    if (abs(matrix_data(1, 2) - 2.0_real64) > 1.0e-12_real64) error stop 23\n    print *, 'ok'\n  end subroutine show\nend module m\n\nprogram p\n  use m\n  use iso_fortran_env, only: real64\n  implicit none\n  call show(reshape([1.0_real64, 3.0_real64, 2.0_real64, 4.0_real64], [2, 2]))\nend program p\n",
        "f90",
    );
    let out = unique_path("reshape_array_actual_subroutine", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-O2", "-o", out.to_str().unwrap()])
        .output()
        .expect("reshape subroutine actual compile failed to spawn");
    assert!(
        compile.status.success(),
        "reshape subroutine actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("reshape subroutine actual run failed");
    assert!(
        run.status.success(),
        "reshape subroutine actual run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected reshape subroutine actual output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn real_intrinsic_honors_named_kind_argument_for_integer_inputs() {
    let src = write_program(
        "program p\n  use iso_fortran_env, only: real64\n  implicit none\n  real(real64) :: x\n  x = real(nint(3.5_real64), real64)\n  if (abs(x - 4.0_real64) > 1.0e-12_real64) error stop 1\n  x = real(nint(3.4_real64), real64)\n  if (abs(x - 3.0_real64) > 1.0e-12_real64) error stop 2\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("real_named_kind_integer_input", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-O2", "-o", out.to_str().unwrap()])
        .output()
        .expect("real intrinsic kind compile failed to spawn");
    assert!(
        compile.status.success(),
        "real intrinsic kind compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("real intrinsic kind run failed");
    assert!(
        run.status.success(),
        "real intrinsic kind run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("ok"), "unexpected stdout: {}", stdout);

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn deferred_local_zero_len_buffer_substring_preserves_written_bytes() {
    let src = write_program(
        "module m\ncontains\n  function build(prompt) result(expanded)\n    character(len=*), intent(in) :: prompt\n    character(len=:), allocatable :: expanded\n    character(len=:), allocatable :: result\n    integer :: i, j\n    allocate(character(len=len(prompt) * 2 + 8) :: result)\n    result = ''\n    i = 1\n    j = 1\n    do while (i <= len(prompt))\n      result(j:j) = prompt(i:i)\n      i = i + 1\n      j = j + 1\n    end do\n    expanded = result(1:j-1)\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  character(len=:), allocatable :: s\n  s = build('+ ')\n  if (len(s) /= 2) error stop 1\n  if (s /= '+ ') error stop 2\n  print *, s\nend program\n",
        "f90",
    );
    let out = unique_path("deferred_local_zero_len_substring", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("deferred local zero-len substring compile failed to spawn");
    assert!(
        compile.status.success(),
        "deferred local zero-len substring compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("deferred local zero-len substring run failed");
    assert!(
        run.status.success(),
        "deferred local zero-len substring run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("+"),
        "unexpected deferred local zero-len substring output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn deferred_char_component_substring_reads_back_written_chars() {
    let src = write_program(
        "module m\n  implicit none\n  type :: state_t\n    character(len=:), allocatable :: search_string\n    integer :: search_length = 0\n  end type\ncontains\n  subroutine init_state(state)\n    type(state_t), intent(inout) :: state\n    allocate(character(len=64) :: state%search_string)\n    state%search_string = ''\n    state%search_length = 0\n  end subroutine\n\n  subroutine set_search_char(state, pos, ch)\n    type(state_t), intent(inout) :: state\n    integer, intent(in) :: pos\n    character, intent(in) :: ch\n    state%search_string(pos:pos) = ch\n  end subroutine\n\n  subroutine get_search_string(state, str, slen)\n    type(state_t), intent(in) :: state\n    character(len=*), intent(out) :: str\n    integer, intent(in) :: slen\n    integer :: j\n    str = ''\n    if (slen <= 0) return\n    do j = 1, min(slen, len(str))\n      str(j:j) = state%search_string(j:j)\n    end do\n  end subroutine\nend module\n\nprogram p\n  use m\n  implicit none\n  type(state_t) :: state\n  character(len=64) :: buf\n  call init_state(state)\n  state%search_length = 4\n  call set_search_char(state, 1, 'f')\n  call set_search_char(state, 2, 'i')\n  call set_search_char(state, 3, 'n')\n  call set_search_char(state, 4, 'd')\n  call get_search_string(state, buf, state%search_length)\n  if (buf(:state%search_length) /= 'find') error stop 1\n  print *, buf(:state%search_length)\nend program\n",
        "f90",
    );
    let out = unique_path("deferred_char_component_substring_reads", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("deferred char component substring read compile failed to spawn");
    assert!(
        compile.status.success(),
        "deferred char component substring read compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("deferred char component substring read run failed");
    assert!(
        run.status.success(),
        "deferred char component substring read run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("find"),
        "unexpected deferred char component substring read output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn local_parameter_from_range_sizes_fixed_char_before_substring_assignment() {
    let src = write_program(
        "module m\ncontains\n  function f(val) result(s)\n    integer, intent(in) :: val\n    character(len=:), allocatable :: s\n    integer, parameter :: buffer_len = range(val)+2\n    character(len=buffer_len) :: buffer\n    buffer = '       1234'\n    s = buffer(8:)\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  character(len=:), allocatable :: s\n  s = f(-1234)\n  if (len(s) /= 4) error stop 1\n  if (s /= '1234') error stop 2\n  print *, s\nend program\n",
        "f90",
    );
    let out = unique_path("local_parameter_range_substring", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("local parameter range substring compile failed to spawn");
    assert!(
        compile.status.success(),
        "local parameter range substring compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("local parameter range substring run failed");
    assert!(
        run.status.success(),
        "local parameter range substring run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("1234"),
        "unexpected local parameter range substring output: {}",
        stdout
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
fn bind_c_c_ptr_value_and_i64_values_preserve_scalar_call_abi() {
    let dir = unique_dir("bind_c_c_ptr_i64_value_args");
    let c_src = write_program_in(
        &dir,
        "check_scan_args.c",
        "#include <stdint.h>\n\nint64_t check_scan_args(const char *buf, int64_t len, int64_t start, char needle) {\n    if (buf == 0) return -11;\n    if (len != 11) return -12;\n    if (start != 0) return -13;\n    if ((unsigned char)needle != 10) return -14;\n    if (buf[0] != 'h') return -15;\n    if (buf[5] != '\\n') return -16;\n    return 5;\n}\n",
    );
    let c_obj = dir.join("check_scan_args.o");
    compile_c_object(&c_src, &c_obj);

    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_ptr, c_char, c_int64_t, c_loc\n  implicit none\n  interface\n    function check_scan_args(buf, lenv, startv, needle) result(pos) bind(C, name='check_scan_args')\n      import :: c_ptr, c_char, c_int64_t\n      type(c_ptr), value :: buf\n      integer(c_int64_t), value :: lenv\n      integer(c_int64_t), value :: startv\n      character(kind=c_char), value :: needle\n      integer(c_int64_t) :: pos\n    end function\n  end interface\n  character(kind=c_char), target :: buf(11)\n  integer(c_int64_t) :: pos\n\n  buf = [achar(104, kind=c_char), achar(101, kind=c_char), achar(108, kind=c_char), &\n         achar(108, kind=c_char), achar(111, kind=c_char), achar(10, kind=c_char), &\n         achar(119, kind=c_char), achar(111, kind=c_char), achar(114, kind=c_char), &\n         achar(108, kind=c_char), achar(100, kind=c_char)]\n\n  pos = check_scan_args(c_loc(buf(1)), 11_c_int64_t, 0_c_int64_t, achar(10, kind=c_char))\n  if (pos /= 5_c_int64_t) error stop 1\n  print *, 'ok'\nend program\n",
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
        .expect("bind(c) c_ptr+i64 value object compile failed to spawn");
    assert!(
        compile_obj.status.success(),
        "bind(c) c_ptr+i64 value object should compile: {}",
        String::from_utf8_lossy(&compile_obj.stderr)
    );

    let exe = dir.join("bind_c_c_ptr_i64_value_args.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            main_obj.to_str().unwrap(),
            c_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) c_ptr+i64 value link failed to spawn");
    assert!(
        link.status.success(),
        "bind(c) c_ptr+i64 value objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("bind(c) c_ptr+i64 value run failed");
    assert!(
        run.status.success(),
        "bind(c) c_ptr+i64 values should preserve the scalar call ABI: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "bind(c) c_ptr+i64 values should arrive at the C callee unchanged: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bind_c_c_ptr_value_and_i64_values_survive_wrapper_dummy_call() {
    let dir = unique_dir("bind_c_c_ptr_i64_wrapper");
    let c_src = write_program_in(
        &dir,
        "check_scan_args.c",
        "#include <stdint.h>\n\nint64_t check_scan_args(const char *buf, int64_t len, int64_t start, char needle) {\n    if (buf == 0) return -11;\n    if (len != 11) return -12;\n    if (start != 0) return -13;\n    if ((unsigned char)needle != 10) return -14;\n    if (buf[0] != 'h') return -15;\n    if (buf[5] != '\\n') return -16;\n    return 5;\n}\n",
    );
    let c_obj = dir.join("check_scan_args.o");
    compile_c_object(&c_src, &c_obj);

    let src = write_program_in(
        &dir,
        "main.f90",
        "module m\n  use iso_c_binding, only: c_ptr, c_char, c_int64_t\n  implicit none\n  interface\n    function check_scan_args(buf, lenv, startv, needle) result(pos) bind(C, name='check_scan_args')\n      import :: c_ptr, c_char, c_int64_t\n      type(c_ptr), value :: buf\n      integer(c_int64_t), value :: lenv\n      integer(c_int64_t), value :: startv\n      character(kind=c_char), value :: needle\n      integer(c_int64_t) :: pos\n    end function\n  end interface\ncontains\n  function wrapper(buf, lenv, startv, needle) result(pos)\n    type(c_ptr), intent(in) :: buf\n    integer(c_int64_t), intent(in) :: lenv, startv\n    character(len=1), intent(in) :: needle\n    integer(c_int64_t) :: pos\n    pos = check_scan_args(buf, lenv, startv, char(ichar(needle), kind=c_char))\n  end function\nend module\nprogram p\n  use iso_c_binding, only: c_ptr, c_char, c_int64_t, c_loc\n  use m\n  implicit none\n  character(kind=c_char), target :: buf(11)\n  integer(c_int64_t) :: pos\n\n  buf = [achar(104, kind=c_char), achar(101, kind=c_char), achar(108, kind=c_char), &\n         achar(108, kind=c_char), achar(111, kind=c_char), achar(10, kind=c_char), &\n         achar(119, kind=c_char), achar(111, kind=c_char), achar(114, kind=c_char), &\n         achar(108, kind=c_char), achar(100, kind=c_char)]\n\n  pos = wrapper(c_loc(buf(1)), 11_c_int64_t, 0_c_int64_t, char(10))\n  if (pos /= 5_c_int64_t) error stop 1\n  print *, 'ok'\nend program\n",
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
        .expect("bind(c) wrapper object compile failed to spawn");
    assert!(
        compile_obj.status.success(),
        "bind(c) wrapper object should compile: {}",
        String::from_utf8_lossy(&compile_obj.stderr)
    );

    let exe = dir.join("bind_c_c_ptr_i64_wrapper.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            main_obj.to_str().unwrap(),
            c_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) wrapper link failed to spawn");
    assert!(
        link.status.success(),
        "bind(c) wrapper objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("bind(c) wrapper run failed");
    assert!(
        run.status.success(),
        "bind(c) wrapper should preserve c_ptr and i64 dummy values: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "bind(c) wrapper should forward the original scalar values unchanged: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bind_c_c_char_function_result_round_trips_through_wrapper_module() {
    let dir = unique_dir("bind_c_c_char_result");
    let c_src = write_program_in(
        &dir,
        "check.c",
        "#include <stddef.h>\n\nvoid *get_static_buf(void) {\n    static char buf[] = \"echo hi\";\n    return buf;\n}\n\nchar buffer_get_char(void *buf, size_t pos) {\n    return ((char *)buf)[pos];\n}\n",
    );
    let c_obj = dir.join("check.o");
    compile_c_object(&c_src, &c_obj);

    let mod_src = write_program_in(
        &dir,
        "c_strings.f90",
        "module c_strings\n  use iso_c_binding, only: c_ptr, c_null_ptr, c_size_t, c_char, c_associated\n  implicit none\n  type :: c_string_buffer\n    type(c_ptr) :: handle = c_null_ptr\n  end type\n  interface\n    function get_static_buf_c() result(buf) bind(C, name='get_static_buf')\n      import :: c_ptr\n      type(c_ptr) :: buf\n    end function\n    function buffer_get_char_c(buf, pos) result(ch) bind(C, name='buffer_get_char')\n      import :: c_ptr, c_size_t, c_char\n      type(c_ptr), value :: buf\n      integer(c_size_t), value :: pos\n      character(kind=c_char) :: ch\n    end function\n  end interface\ncontains\n  function c_string_create() result(buf)\n    type(c_string_buffer) :: buf\n    buf%handle = get_static_buf_c()\n  end function\n\n  function c_string_get_char(buf, pos) result(ch)\n    type(c_string_buffer), intent(in) :: buf\n    integer, intent(in) :: pos\n    character(len=1) :: ch\n    if (.not. c_associated(buf%handle)) then\n      ch = ' '\n      return\n    end if\n    ch = buffer_get_char_c(buf%handle, int(pos - 1, c_size_t))\n  end function\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use c_strings\n  implicit none\n  type(c_string_buffer) :: buf\n  character(len=1) :: ch\n  buf = c_string_create()\n  ch = c_string_get_char(buf, 1)\n  if (ch /= 'e') error stop 1\n  ch = c_string_get_char(buf, 5)\n  if (ch /= ' ') error stop 2\n  print *, 'ok'\nend program\n",
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
        .expect("bind(c) c_char result module compile failed to spawn");
    assert!(
        compile_mod.status.success(),
        "bind(c) c_char result wrapper module should compile: {}",
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
        .expect("bind(c) c_char result user compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "bind(c) c_char result user should compile: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("bind_c_c_char_result.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            main_obj.to_str().unwrap(),
            mod_obj.to_str().unwrap(),
            c_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("bind(c) c_char result link failed to spawn");
    assert!(
        link.status.success(),
        "bind(c) c_char result objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("bind(c) c_char result run failed");
    assert!(
        run.status.success(),
        "bind(c) c_char result should run: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "bind(c) c_char function result should round-trip through the wrapper module: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocatable_c_char_array_function_result_passes_bind_c_assumed_size_dummy() {
    let dir = unique_dir("alloc_c_char_result_bind_c");
    let c_src = write_program_in(
        &dir,
        "check.c",
        "#include <string.h>\n\nvoid ccheck(const char *s, int *ok) {\n    *ok = strcmp(s, \"printf 'hello'\") == 0 ? 1 : 0;\n}\n",
    );
    let c_obj = dir.join("check.o");
    compile_c_object(&c_src, &c_obj);

    let src = write_program_in(
        &dir,
        "main.f90",
        "module m\n  use iso_c_binding, only: c_char, c_int, c_null_char\n  implicit none\n  interface\n    subroutine ccheck(s, ok) bind(C, name='ccheck')\n      import :: c_char, c_int\n      character(kind=c_char), intent(in) :: s(*)\n      integer(c_int), intent(out) :: ok\n    end subroutine ccheck\n  end interface\ncontains\n  function to_c_string(str) result(buf)\n    character(len=*), intent(in) :: str\n    character(kind=c_char), allocatable :: buf(:)\n    integer :: i, n\n    n = len(str)\n    allocate(buf(n + 1))\n    do i = 1, n\n      buf(i) = str(i:i)\n    end do\n    buf(n + 1) = c_null_char\n  end function to_c_string\n\n  subroutine run(cmdline)\n    character(len=*), intent(in) :: cmdline\n    character(kind=c_char), allocatable :: c_command_line(:)\n    integer(c_int) :: ok\n    c_command_line = to_c_string(cmdline)\n    call ccheck(c_command_line, ok)\n    if (ok /= 1_c_int) error stop 1\n  end subroutine run\nend module m\nprogram p\n  use m\n  implicit none\n  call run(\"printf 'hello'\")\n  print *, 'ok'\nend program p\n",
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
        .expect("allocatable c_char result bind(c) object compile failed to spawn");
    assert!(
        compile_obj.status.success(),
        "allocatable c_char result bind(c) should compile to an object: {}",
        String::from_utf8_lossy(&compile_obj.stderr)
    );

    let exe = dir.join("alloc_c_char_result_bind_c.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            main_obj.to_str().unwrap(),
            c_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("allocatable c_char result bind(c) link failed to spawn");
    assert!(
        link.status.success(),
        "allocatable c_char result bind(c) objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("allocatable c_char result bind(c) run failed");
    assert!(
        run.status.success(),
        "allocatable c_char result bind(c) should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected allocatable c_char result bind(c) output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocatable_c_char_array_result_assigns_into_intent_out_dummy() {
    let dir = unique_dir("alloc_c_char_result_intent_out");
    let src = write_program_in(
        &dir,
        "main.f90",
        "module m\n  use iso_c_binding, only: c_char, c_int, c_null_char\n  implicit none\ncontains\n  function empty_c_string() result(buf)\n    character(kind=c_char), allocatable :: buf(:)\n    allocate(buf(1))\n    buf(1) = c_null_char\n  end function empty_c_string\n\n  subroutine pack(values, stride, buffer)\n    character(len=*), intent(in) :: values(:)\n    integer(c_int), intent(out) :: stride\n    character(kind=c_char), allocatable, intent(out) :: buffer(:)\n    if (size(values) == 0) then\n      stride = 0_c_int\n      buffer = empty_c_string()\n      return\n    end if\n    error stop 99\n  end subroutine pack\nend module m\n\nprogram p\n  use iso_c_binding, only: c_char, c_int, c_null_char\n  use m, only: pack\n  implicit none\n  character(len=1), allocatable :: values(:)\n  character(kind=c_char), allocatable :: buffer(:)\n  integer(c_int) :: stride\n  allocate(character(len=1) :: values(0))\n  call pack(values, stride, buffer)\n  if (stride /= 0_c_int) error stop 1\n  if (.not. allocated(buffer)) error stop 2\n  if (size(buffer) /= 1) error stop 3\n  if (buffer(1) /= c_null_char) error stop 4\n  print *, 'ok'\nend program p\n",
    );

    let exe = dir.join("alloc_c_char_result_intent_out.bin");
    let compile = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([src.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("allocatable c_char intent(out) compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocatable c_char intent(out) should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("allocatable c_char intent(out) run failed");
    assert!(
        run.status.success(),
        "allocatable c_char intent(out) should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected allocatable c_char intent(out) output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deferred_char_array_component_len_survives_function_result_and_dummy_pass() {
    let dir = unique_dir("deferred_char_array_component_len");
    let src = write_program_in(
        &dir,
        "main.f90",
        "module m\n  implicit none\n  type :: process_command\n    character(len=:), allocatable :: program\n    character(len=:), allocatable :: argv(:)\n  end type\ncontains\n  function command(program, argv) result(cmd)\n    character(len=*), intent(in) :: program\n    character(len=*), intent(in), optional :: argv(:)\n    type(process_command) :: cmd\n    integer :: arg_len\n    integer :: i\n    cmd%program = program\n    if (present(argv)) then\n      arg_len = max_string_length(argv)\n      allocate(character(len=arg_len) :: cmd%argv(size(argv)))\n      do i = 1, size(argv)\n        cmd%argv(i) = argv(i)\n      end do\n    else\n      allocate(character(len=1) :: cmd%argv(0))\n    end if\n  end function command\n\n  integer function max_string_length(values) result(max_len)\n    character(len=*), intent(in) :: values(:)\n    integer :: i\n    max_len = 1\n    do i = 1, size(values)\n      max_len = max(max_len, len(values(i)))\n    end do\n  end function max_string_length\n\n  subroutine consume(cmd)\n    type(process_command), intent(in) :: cmd\n    if (size(cmd%argv) /= 2) error stop 1\n    if (len(cmd%argv) /= 26) error stop 2\n    if (trim(cmd%argv(1)) /= '%s') error stop 3\n    if (trim(cmd%argv(2)) /= \"left | sed 's/left/right/'\") error stop 4\n  end subroutine consume\nend module m\n\nprogram p\n  use m\n  implicit none\n  call consume(command('printf', [character(len=26) :: '%s', \"left | sed 's/left/right/'\"]))\n  print *, 'ok'\nend program p\n",
    );

    let exe = dir.join("deferred_char_array_component_len.bin");
    let compile = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([src.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("deferred char array component len compile failed to spawn");
    assert!(
        compile.status.success(),
        "deferred char array component len should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("deferred char array component len run failed");
    assert!(
        run.status.success(),
        "deferred char array component len should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected deferred char array component len output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn absent_optional_char_array_forwarding_uses_zero_hidden_length() {
    let dir = unique_dir("optional_char_array_hidden_len");
    let src = write_program_in(
        &dir,
        "main.f90",
        "module m\n  implicit none\n  type :: result_t\n    integer :: code = 0\n  end type result_t\ncontains\n  subroutine inner(program, argv, size)\n    character(len=*), intent(in) :: program\n    character(len=*), intent(in), optional :: argv(:)\n    integer, intent(in), optional :: size\n    if (len_trim(program) == 0) error stop 11\n    if (present(argv)) error stop 12\n    if (present(size)) error stop 13\n  end subroutine inner\n\n  type(result_t) function outer(program, argv, size) result(session)\n    character(len=*), intent(in) :: program\n    character(len=*), intent(in), optional :: argv(:)\n    integer, intent(in), optional :: size\n    if (len_trim(program) == 0) then\n      session%code = 1\n      return\n    end if\n    call inner(program, argv, size)\n    session%code = 2\n  end function outer\nend module m\n\nprogram p\n  use m\n  implicit none\n  type(result_t) :: session\n  session = outer('abc')\n  if (session%code /= 2) error stop 1\n  print *, 'ok'\nend program p\n",
    );

    let exe = dir.join("optional_char_array_hidden_len.bin");
    let compile = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([src.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("optional char array hidden len compile failed to spawn");
    assert!(
        compile.status.success(),
        "optional char array hidden len should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("optional char array hidden len run failed");
    assert!(
        run.status.success(),
        "optional char array hidden len should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected optional char array hidden len output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_absent_optional_char_array_forwarding_survives_function_result_wrapper() {
    let dir = unique_dir("cross_tu_optional_char_array_forward");
    let types_src = write_program_in(
        &dir,
        "types.f90",
        "module types\n  implicit none\n  type :: terminal_size\n    integer :: rows = 24\n    integer :: cols = 80\n  end type terminal_size\n  type :: session_t\n    integer :: code = -1\n  end type session_t\nend module types\n",
    );
    let posix_src = write_program_in(
        &dir,
        "posix.f90",
        "module posix\n  use types, only: terminal_size, session_t\n  implicit none\ncontains\n  subroutine spawn_posix_pty(program, argv, term_size, session)\n    character(len=*), intent(in) :: program\n    character(len=*), intent(in), optional :: argv(:)\n    type(terminal_size), intent(in) :: term_size\n    type(session_t), intent(inout) :: session\n    if (len_trim(program) == 0) error stop 11\n    if (present(argv)) error stop 12\n    if (term_size%rows /= 24 .or. term_size%cols /= 80) error stop 13\n    session%code = len_trim(program)\n  end subroutine spawn_posix_pty\nend module posix\n",
    );
    let api_src = write_program_in(
        &dir,
        "api.f90",
        "module api\n  use posix, only: spawn_posix_pty\n  use types, only: terminal_size, session_t\n  implicit none\ncontains\n  function default_terminal_size() result(size)\n    type(terminal_size) :: size\n  end function default_terminal_size\n\n  logical function valid_terminal_size(size) result(valid)\n    type(terminal_size), intent(in) :: size\n    valid = size%rows > 0 .and. size%cols > 0\n  end function valid_terminal_size\n\n  function spawn_pty(program, argv, size) result(session)\n    character(len=*), intent(in) :: program\n    character(len=*), intent(in), optional :: argv(:)\n    type(terminal_size), intent(in), optional :: size\n    type(session_t) :: session\n    type(terminal_size) :: launch_size\n    session%code = -1\n    if (len_trim(program) == 0) then\n      session%code = 1\n      return\n    end if\n    if (present(size)) then\n      launch_size = size\n    else\n      launch_size = default_terminal_size()\n    end if\n    if (.not. valid_terminal_size(launch_size)) then\n      session%code = 2\n      return\n    end if\n    call spawn_posix_pty(program, argv, launch_size, session)\n  end function spawn_pty\nend module api\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use api, only: spawn_pty\n  use types, only: session_t\n  implicit none\n  type(session_t) :: session\n  session = spawn_pty('fgof-pty-missing-command')\n  if (session%code /= len('fgof-pty-missing-command')) error stop 21\n  print *, 'ok'\nend program p\n",
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
        .expect("types module compile failed to spawn");
    assert!(
        compile_types.status.success(),
        "types module should compile: {}",
        String::from_utf8_lossy(&compile_types.stderr)
    );

    let posix_obj = dir.join("posix.o");
    let compile_posix = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            posix_src.to_str().unwrap(),
            "-o",
            posix_obj.to_str().unwrap(),
        ])
        .output()
        .expect("posix module compile failed to spawn");
    assert!(
        compile_posix.status.success(),
        "posix module should compile: {}",
        String::from_utf8_lossy(&compile_posix.stderr)
    );

    let api_obj = dir.join("api.o");
    let compile_api = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            api_src.to_str().unwrap(),
            "-o",
            api_obj.to_str().unwrap(),
        ])
        .output()
        .expect("api module compile failed to spawn");
    assert!(
        compile_api.status.success(),
        "api module should compile: {}",
        String::from_utf8_lossy(&compile_api.stderr)
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

    let exe = dir.join("cross_tu_optional_char_array_forward.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            types_obj.to_str().unwrap(),
            posix_obj.to_str().unwrap(),
            api_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("cross-tu optional char array link failed to spawn");
    assert!(
        link.status.success(),
        "cross-tu optional char array objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("cross-tu optional char array run failed");
    assert!(
        run.status.success(),
        "cross-tu optional char array binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected cross-tu optional char array output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn stream_unformatted_read_into_allocatable_char_scalar_preserves_bytes() {
    let dir = unique_dir("stream_read_alloc_char_scalar");
    let input_path = dir.join("external_hello.txt");
    std::fs::write(&input_path, b"hello").expect("failed to seed stream input");
    let input_path_str = input_path.to_str().unwrap().replace('\\', "\\\\");
    let src = write_program_in(
        &dir,
        "main.f90",
        &format!(
            "program p\n  implicit none\n  character(len=:), allocatable :: text\n  integer :: unit\n  integer :: ios\n  integer :: file_size\n  open(newunit=unit, file='{}', status='old', access='stream', form='unformatted', action='read', iostat=ios)\n  if (ios /= 0) error stop 1\n  inquire(unit=unit, size=file_size)\n  allocate(character(len=file_size) :: text)\n  read(unit, iostat=ios) text\n  if (ios /= 0) error stop 2\n  if (text /= 'hello') error stop 3\n  close(unit)\n  print *, 'ok'\nend program p\n",
            input_path_str
        ),
    );

    let exe = dir.join("stream_read_alloc_char_scalar.bin");
    let compile = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([src.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("stream read alloc char scalar compile failed to spawn");
    assert!(
        compile.status.success(),
        "stream read alloc char scalar should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("stream read alloc char scalar run failed");
    assert!(
        run.status.success(),
        "stream read alloc char scalar should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected stream read alloc char scalar output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn logical_intrinsic_kind_argument_compiles_and_runs() {
    let dir = unique_dir("logical_kind_intrinsic");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_bool\n  implicit none\n  logical :: bf\n  logical(c_bool) :: out\n  bf = .true.\n  out = logical(bf, c_bool)\n  if (.not. out) error stop 1\n  print *, 'ok'\nend program p\n",
    );

    let exe = dir.join("logical_kind_intrinsic.bin");
    let compile = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([src.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("logical kind intrinsic compile failed to spawn");
    assert!(
        compile.status.success(),
        "logical kind intrinsic should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("logical kind intrinsic run failed");
    assert!(
        run.status.success(),
        "logical kind intrinsic should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected logical kind intrinsic output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ieee_value_quiet_nan_from_intrinsic_module_compiles_and_runs() {
    let dir = unique_dir("ieee_value_quiet_nan");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use, intrinsic :: ieee_arithmetic, only : ieee_value, ieee_quiet_nan, ieee_is_nan\n  implicit none\n  real :: x\n  x = ieee_value(0.0, ieee_quiet_nan)\n  if (.not. ieee_is_nan(x)) error stop 1\n  print *, 'ok'\nend program p\n",
    );

    let exe = dir.join("ieee_value_quiet_nan.bin");
    let compile = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([src.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("ieee quiet nan compile failed to spawn");
    assert!(
        compile.status.success(),
        "ieee quiet nan should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("ieee quiet nan run failed");
    assert!(
        run.status.success(),
        "ieee quiet nan should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected ieee quiet nan output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ieee_value_positive_inf_from_intrinsic_module_is_not_finite() {
    let dir = unique_dir("ieee_value_positive_inf");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use, intrinsic :: ieee_arithmetic, only : ieee_value, ieee_positive_inf, ieee_is_finite\n  implicit none\n  real :: x\n  x = ieee_value(0.0, ieee_positive_inf)\n  if (ieee_is_finite(x)) error stop 1\n  if (x <= huge(0.0)) error stop 2\n  print *, 'ok'\nend program p\n",
    );

    let exe = dir.join("ieee_value_positive_inf.bin");
    let compile = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([src.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("ieee positive inf compile failed to spawn");
    assert!(
        compile.status.success(),
        "ieee positive inf should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("ieee positive inf run failed");
    assert!(
        run.status.success(),
        "ieee positive inf should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected ieee positive inf output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn contained_char_function_in_comparison_uses_internal_call_target() {
    let dir = unique_dir("contained_char_fn_compare");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  call write_text_file('x', 'hello')\n  if (read_text_file('x') /= 'hello') error stop 1\n  print *, 'ok'\ncontains\n  subroutine write_text_file(path, text)\n    character(len=*), intent(in) :: path\n    character(len=*), intent(in) :: text\n    integer :: unit\n    open(newunit=unit, file=path, status='replace', action='write')\n    write(unit, '(A)') text\n    close(unit)\n  end subroutine write_text_file\n\n  function read_text_file(path) result(text)\n    character(len=*), intent(in) :: path\n    character(len=:), allocatable :: text\n    character(len=256) :: buffer\n    integer :: unit\n    open(newunit=unit, file=path, status='old', action='read')\n    read(unit, '(A)') buffer\n    close(unit)\n    text = trim(buffer)\n  end function read_text_file\nend program p\n",
    );

    let exe = dir.join("contained_char_fn_compare.bin");
    let compile = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([src.to_str().unwrap(), "-o", exe.to_str().unwrap()])
        .output()
        .expect("contained char fn compare compile failed to spawn");
    assert!(
        compile.status.success(),
        "contained char fn compare should compile and link: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .current_dir(&dir)
        .output()
        .expect("contained char fn compare run failed");
    assert!(
        run.status.success(),
        "contained char fn compare should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected contained char fn compare output: {}",
        stdout
    );

    let _ = std::fs::remove_file(dir.join("x"));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn close_status_delete_removes_file() {
    let path = unique_path("close_status_delete", "txt");
    let src = write_program(
        &format!(
            "program p\n  implicit none\n  integer :: unit, ios\n  open(newunit=unit, file='{}', status='replace', action='write', iostat=ios)\n  if (ios /= 0) error stop 1\n  write(unit, '(a)', iostat=ios) 'hello'\n  if (ios /= 0) error stop 2\n  close(unit, status='delete', iostat=ios)\n  if (ios /= 0) error stop 3\n  print *, 'ok'\nend program\n",
            path.display()
        ),
        "f90",
    );
    let out = unique_path("close_status_delete", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("close status delete compile failed to spawn");
    assert!(
        compile.status.success(),
        "close status delete compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("close status delete run failed");
    assert!(
        run.status.success(),
        "close status delete run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        !path.exists(),
        "close(status='delete') should remove the file: {}",
        path.display()
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&path);
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
fn allocated_intrinsic_on_class_component_chain_compiles_and_runs() {
    let src = write_program(
        "module m\n  implicit none\n  type, abstract :: base_t\n    character(len=:), allocatable :: key\n  end type base_t\n  type, extends(base_t) :: child_t\n    integer :: x = 0\n  end type child_t\n  type :: node_t\n    class(base_t), allocatable :: val\n  end type node_t\n  type :: container_t\n    type(node_t), allocatable :: lst(:)\n  end type container_t\ncontains\n  logical function has_key(c)\n    type(container_t), intent(in) :: c\n    has_key = allocated(c%lst(1)%val%key)\n  end function has_key\nend module m\nprogram p\n  use m\n  implicit none\n  type(container_t) :: c\n  allocate(c%lst(1))\n  allocate(child_t :: c%lst(1)%val)\n  c%lst(1)%val%key = 'abc'\n  if (.not. has_key(c)) error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("allocated_class_component_chain", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("allocated class component chain compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocated class component chain should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("allocated class component chain run failed");
    assert!(
        run.status.success(),
        "allocated class component chain should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected allocated class component chain output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn explicit_shape_runtime_bound_function_result_array_compiles_and_runs() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: idx\n  idx = strstr('ababa', 'aba')\n  if (idx /= 1) error stop 1\n  print *, 'ok'\ncontains\n  integer function strstr(string, pattern) result(res)\n    character(*), intent(in) :: string\n    character(*), intent(in) :: pattern\n    integer :: lps_array(len(pattern))\n    integer :: res, s_i, p_i, length_string, length_pattern\n    res = 0\n    length_string = len(string)\n    length_pattern = len(pattern)\n    if (length_pattern > 0 .and. length_pattern <= length_string) then\n      lps_array = compute_lps(pattern)\n      s_i = 1\n      p_i = 1\n      do while (s_i <= length_string)\n        if (string(s_i:s_i) == pattern(p_i:p_i)) then\n          if (p_i == length_pattern) then\n            res = s_i - length_pattern + 1\n            exit\n          end if\n          s_i = s_i + 1\n          p_i = p_i + 1\n        else if (p_i > 1) then\n          p_i = lps_array(p_i - 1) + 1\n        else\n          s_i = s_i + 1\n        end if\n      end do\n    end if\n  contains\n    pure function compute_lps(string) result(lps_array)\n      character(*), intent(in) :: string\n      integer :: lps_array(len(string))\n      integer :: i, j, length_string\n      length_string = len(string)\n      if (length_string > 0) then\n        lps_array(1) = 0\n        i = 2\n        j = 1\n        do while (i <= length_string)\n          if (string(j:j) == string(i:i)) then\n            lps_array(i) = j\n            i = i + 1\n            j = j + 1\n          else if (j > 1) then\n            j = lps_array(j - 1) + 1\n          else\n            lps_array(i) = 0\n            i = i + 1\n          end if\n        end do\n      end if\n    end function compute_lps\n  end function strstr\nend program\n",
        "f90",
    );
    let out = unique_path("runtime_bound_result_array", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("runtime-bound result array compile failed to spawn");
    assert!(
        compile.status.success(),
        "runtime-bound explicit-shape result array should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("runtime-bound result array run failed");
    assert!(
        run.status.success(),
        "runtime-bound explicit-shape result array should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected runtime-bound result array output: {}",
        String::from_utf8_lossy(&run.stdout)
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
fn automatic_component_array_bound_size_lowers_without_raw_symbol() {
    let src = write_program(
        "module m\n  implicit none\n  type :: state_set_t\n    integer(8) :: bits(4) = 0_8\n  contains\n    procedure :: f\n  end type\ncontains\n  subroutine f(state_set)\n    type(state_set_t), intent(inout) :: state_set\n    integer(8) :: original_bits(size(state_set%bits))\n    original_bits = state_set%bits\n    print *, size(original_bits)\n  end subroutine\nend module\n",
        "f90",
    );
    let out = unique_path("auto_component_bound_size", "o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .env("NO_COLOR", "1")
        .output()
        .expect("automatic component bound size compile failed to spawn");
    assert!(
        compile.status.success(),
        "automatic component bound size compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let undefined = undefined_symbols(&out);
    assert!(
        undefined.iter().any(|sym| sym == "_afs_array_size"),
        "automatic component bound size() should still lower through afs_array_size: {:?}",
        undefined
    );
    assert!(
        !undefined.iter().any(|sym| sym == "_size"),
        "automatic component bound size() should not escape as a raw symbol: {:?}",
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
fn typed_allocate_char_len_from_derived_component_expr_runs() {
    let src = write_program(
        "module m\n  use, intrinsic :: iso_c_binding\n  implicit none\n  type :: line_info_t\n    integer(c_size_t) :: start_pos = 0\n    integer(c_size_t) :: length = 0\n  end type line_info_t\ncontains\n  function get_line_text(data_ptr, data_size, info) result(line)\n    type(c_ptr), intent(in) :: data_ptr\n    integer(c_size_t), intent(in) :: data_size\n    type(line_info_t), intent(in) :: info\n    character(len=:), allocatable :: line\n    character(len=1, kind=c_char), pointer :: file_data(:)\n    integer :: i\n    if (info%length == 0) then\n      line = ''\n      return\n    end if\n    call c_f_pointer(data_ptr, file_data, [data_size])\n    allocate(character(len=info%length) :: line)\n    do i = 1, int(info%length)\n      line(i:i) = file_data(info%start_pos + i)\n    end do\n  end function get_line_text\nend module m\n\nprogram p\n  use, intrinsic :: iso_c_binding\n  use m\n  implicit none\n  character(kind=c_char), target :: buf(11)\n  type(line_info_t) :: info\n  character(len=:), allocatable :: line\n  buf = [char(104, kind=c_char), char(101, kind=c_char), char(108, kind=c_char), &\n         char(108, kind=c_char), char(111, kind=c_char), char(10, kind=c_char), &\n         char(119, kind=c_char), char(111, kind=c_char), char(114, kind=c_char), &\n         char(108, kind=c_char), char(100, kind=c_char)]\n  info%start_pos = 0\n  info%length = 5\n  line = get_line_text(c_loc(buf(1)), 11_c_size_t, info)\n  if (len(line) /= 5) error stop 1\n  if (line /= 'hello') error stop 2\n  print *, line\nend program p\n",
        "f90",
    );
    let out = unique_path("typed_allocate_char_len_from_component", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("typed allocate char len from component compile failed to spawn");
    assert!(
        compile.status.success(),
        "typed allocate char len from component compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("typed allocate char len from component run failed");
    assert!(
        run.status.success(),
        "typed allocate char len from component should preserve the runtime component length: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("hello"),
        "typed allocate char len from component should return the copied text: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn type_bound_deferred_char_result_preserves_pass_object_and_length() {
    let src = write_program(
        "module m\n  use, intrinsic :: iso_c_binding\n  implicit none\n  type :: line_info_t\n    integer(c_size_t) :: start_pos = 0\n    integer(c_size_t) :: length = 0\n  end type line_info_t\n  type :: holder_t\n    type(c_ptr) :: data = c_null_ptr\n    integer(c_size_t) :: size = 0\n    logical :: is_open = .false.\n  contains\n    procedure :: get_line_text\n  end type holder_t\ncontains\n  function get_line_text(this, info) result(line)\n    class(holder_t), intent(in) :: this\n    type(line_info_t), intent(in) :: info\n    character(len=:), allocatable :: line\n    character(len=1, kind=c_char), pointer :: file_data(:)\n    integer :: i\n    if (.not. this%is_open .or. .not. c_associated(this%data)) then\n      line = ''\n      return\n    end if\n    if (info%length == 0) then\n      line = ''\n      return\n    end if\n    call c_f_pointer(this%data, file_data, [this%size])\n    allocate(character(len=info%length) :: line)\n    do i = 1, int(info%length)\n      line(i:i) = file_data(info%start_pos + i)\n    end do\n  end function get_line_text\nend module m\n\nprogram p\n  use, intrinsic :: iso_c_binding\n  use m\n  implicit none\n  character(kind=c_char), target :: buf(11)\n  type(holder_t) :: holder\n  type(line_info_t) :: info\n  character(len=:), allocatable :: line\n  buf = [char(104, kind=c_char), char(101, kind=c_char), char(108, kind=c_char), &\n         char(108, kind=c_char), char(111, kind=c_char), char(10, kind=c_char), &\n         char(119, kind=c_char), char(111, kind=c_char), char(114, kind=c_char), &\n         char(108, kind=c_char), char(100, kind=c_char)]\n  holder%data = c_loc(buf(1))\n  holder%size = 11\n  holder%is_open = .true.\n  info%start_pos = 0\n  info%length = 5\n  line = holder%get_line_text(info)\n  if (len(line) /= 5) error stop 1\n  if (line /= 'hello') error stop 2\n  print *, line\nend program p\n",
        "f90",
    );
    let out = unique_path("type_bound_deferred_char_result", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("type-bound deferred char result compile failed to spawn");
    assert!(
        compile.status.success(),
        "type-bound deferred char result compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("type-bound deferred char result run failed");
    assert!(
        run.status.success(),
        "type-bound deferred char result should preserve the pass object and result descriptor: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("hello"),
        "type-bound deferred char result should return the copied text: {}",
        stdout
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
fn c_f_pointer_scalar_fixed_c_char_pointer_reads_back_bytes() {
    let src = write_program(
        "program p\n  use iso_c_binding\n  implicit none\n  character(kind=c_char), target :: buf(4)\n  type(c_ptr) :: raw\n  character(kind=c_char, len=4), pointer :: view\n  buf = [achar(97, kind=c_char), achar(98, kind=c_char), achar(99, kind=c_char), c_null_char]\n  raw = c_loc(buf(1))\n  call c_f_pointer(raw, view)\n  if (view(1:1) /= achar(97, kind=c_char)) error stop 1\n  if (view(2:2) /= achar(98, kind=c_char)) error stop 2\n  if (view(3:3) /= achar(99, kind=c_char)) error stop 3\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("c_f_pointer_scalar_char", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("c_f_pointer scalar fixed c_char compile failed to spawn");
    assert!(
        compile.status.success(),
        "c_f_pointer scalar fixed c_char compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("c_f_pointer scalar fixed c_char run failed");
    assert!(
        run.status.success(),
        "c_f_pointer scalar fixed c_char run failed: {}",
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
fn verify_on_indexed_parameter_character_array_element_runs() {
    let src = write_program(
        "program p\n  implicit none\n  character(*), parameter :: toml_base(4) = [ &\n    '0123456789abcdefABCDEF', &\n    '0123456789000000000000', &\n    '0123456700000000000000', &\n    '0100000000000000000000' ]\n  character(1) :: ch\n  integer :: base\n\n  ch = '0'\n  do base = 1, 4\n    if (verify(ch, toml_base(base)) /= 0) error stop base\n  end do\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("verify_param_char_array_elem", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("verify parameter char array compile failed to spawn");
    assert!(
        compile.status.success(),
        "verify parameter char array compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("verify parameter char array run failed");
    assert!(
        run.status.success(),
        "verify parameter char array run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected verify parameter char array output: {}",
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
fn allocatable_fixed_char_actual_to_deferred_char_component_array_round_trips() {
    let src = write_program(
        "program p\n  implicit none\n  type :: simple_command_data_t\n    character(len=:), allocatable :: words(:)\n    integer :: num_words = 0\n  end type simple_command_data_t\n  type :: command_node_t\n    type(simple_command_data_t) :: simple_cmd\n  end type command_node_t\n  type(command_node_t), pointer :: node\n  character(len=32), allocatable :: words(:)\n  allocate(words(2))\n  words(1) = 'echo'\n  words(2) = 'first'\n  node => create_simple_command(words, 2)\n  if (trim(node%simple_cmd%words(1)) /= 'echo') error stop 1\n  if (trim(node%simple_cmd%words(2)) /= 'first') error stop 2\n  print *, trim(node%simple_cmd%words(1)), trim(node%simple_cmd%words(2))\ncontains\n  function create_simple_command(words, num_words) result(node)\n    character(len=*), intent(in) :: words(:)\n    integer, intent(in) :: num_words\n    type(command_node_t), pointer :: node\n    integer :: i\n    allocate(node)\n    allocate(character(len=32) :: node%simple_cmd%words(num_words))\n    node%simple_cmd%num_words = num_words\n    do i = 1, num_words\n      node%simple_cmd%words(i) = words(i)\n    end do\n  end function create_simple_command\nend program p\n",
        "f90",
    );
    let out = unique_path("alloc_fixed_char_to_deferred_component_array", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable fixed-char to deferred component array compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocatable fixed-char to deferred component array compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("allocatable fixed-char to deferred component array run failed");
    assert!(
        run.status.success(),
        "allocatable fixed-char to deferred component array run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("echo") && stdout.contains("first"),
        "unexpected allocatable fixed-char to deferred component array output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_parameter_typed_char_component_allocate_preserves_element_len() {
    let src = write_program(
        "module cfg\n  implicit none\n  integer, parameter :: max_token_len = 4096\nend module cfg\nprogram p\n  use cfg\n  implicit none\n  type :: simple_command_data_t\n    character(len=:), allocatable :: words(:)\n  end type simple_command_data_t\n  type :: command_node_t\n    type(simple_command_data_t), pointer :: simple_cmd => null()\n  end type command_node_t\n  type(command_node_t), pointer :: node\n  allocate(node)\n  allocate(node%simple_cmd)\n  allocate(character(len=max_token_len) :: node%simple_cmd%words(2))\n  node%simple_cmd%words(1) = 'echo'\n  node%simple_cmd%words(2) = 'first'\n  if (trim(node%simple_cmd%words(1)) /= 'echo') error stop 1\n  if (trim(node%simple_cmd%words(2)) /= 'first') error stop 2\n  print *, trim(node%simple_cmd%words(1)), trim(node%simple_cmd%words(2))\nend program p\n",
        "f90",
    );
    let out = unique_path("imported_param_char_component_alloc", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("imported parameter typed char component allocate compile failed to spawn");
    assert!(
        compile.status.success(),
        "imported parameter typed char component allocate compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("imported parameter typed char component allocate run failed");
    assert!(
        run.status.success(),
        "imported parameter typed char component allocate run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("echo") && stdout.contains("first"),
        "unexpected imported parameter typed char component allocate output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn character_len_max_integer_expr_preserves_exact_large_length() {
    let src = write_program(
        "program p\n  implicit none\n  integer(kind=8) :: n\n  character(len=:), allocatable :: s\n  n = 16777217_8\n  allocate(character(len=max(n, 1_8)) :: s)\n  if (len(s) /= n) error stop 1\n  print *, len(s)\nend program p\n",
        "f90",
    );
    let out = unique_path("char_len_max_integer_expr_exact", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("character len max integer expr compile failed to spawn");
    assert!(
        compile.status.success(),
        "character len max integer expr compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("character len max integer expr run failed");
    assert!(
        run.status.success(),
        "character len max integer expr run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("16777217"),
        "unexpected character len max integer expr output: {}",
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
fn utf8_string_literal_preserves_source_bytes_at_runtime() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=:), allocatable :: s\n  s = '├──'\n  if (len(s) /= 9) error stop 1\n  if (len_trim(s) /= 9) error stop 2\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("utf8_string_literal_runtime", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("utf8 string literal compile failed to spawn");
    assert!(
        compile.status.success(),
        "utf8 string literal should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("utf8 string literal run failed");
    assert!(
        run.status.success(),
        "utf8 string literal should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected utf8 string literal output: {}",
        String::from_utf8_lossy(&run.stdout)
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
fn imported_select_case_label_parameter_is_retained() {
    let src = write_program(
        "module kinds_m\n  implicit none\n  integer, parameter :: CMD_SIMPLE = 1\nend module kinds_m\n\nmodule dispatch_m\n  use kinds_m, only: CMD_SIMPLE\n  implicit none\ncontains\n  logical function is_simple(node_type) result(ok)\n    integer, intent(in) :: node_type\n    select case (node_type)\n    case (CMD_SIMPLE)\n      ok = .true.\n    case default\n      ok = .false.\n    end select\n  end function is_simple\nend module dispatch_m\n\nprogram p\n  use dispatch_m\n  implicit none\n  if (.not. is_simple(1)) error stop 1\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("select_case_imported_label_parameter", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("select-case imported label parameter compile failed to spawn");
    assert!(
        compile.status.success(),
        "select-case imported label parameter should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("select-case imported label parameter run failed");
    assert!(
        run.status.success(),
        "select-case imported label parameter should run: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "select-case imported label parameter should preserve CASE constants: {}",
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
fn compile_only_explicit_object_path_keeps_module_in_current_dir() {
    let dir = unique_dir("module_cwd_amod");
    std::fs::create_dir_all(dir.join("src/utils")).expect("mkdir utils");
    std::fs::create_dir_all(dir.join("src/buffer")).expect("mkdir buffer");
    let mod_src = write_program_in(
        &dir.join("src/utils"),
        "utf8_module.f90",
        "module utf8_module\n  implicit none\n  integer, parameter :: UTF8_OK = 7\nend module\n",
    );
    let user_src = write_program_in(
        &dir.join("src/buffer"),
        "text_buffer_module.f90",
        "module text_buffer_module\n  use utf8_module, only: UTF8_OK\n  implicit none\n  integer, parameter :: VALUE = UTF8_OK\nend module\n",
    );
    let mod_obj = dir.join("src/utils/utf8_module.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            mod_src.strip_prefix(&dir).unwrap().to_str().unwrap(),
            "-o",
            mod_obj.strip_prefix(&dir).unwrap().to_str().unwrap(),
        ])
        .output()
        .expect("module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "module compile failed: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );
    assert!(
        dir.join("utf8_module.amod").exists(),
        "compile-only build should emit module file in current dir when -J is absent"
    );

    let user_obj = dir.join("src/buffer/text_buffer_module.o");
    let compile_user = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            user_src.strip_prefix(&dir).unwrap().to_str().unwrap(),
            "-o",
            user_obj.strip_prefix(&dir).unwrap().to_str().unwrap(),
        ])
        .output()
        .expect("consumer compile spawn failed");
    assert!(
        compile_user.status.success(),
        "consumer compile should find cwd module file: {}",
        String::from_utf8_lossy(&compile_user.stderr)
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
fn dash_cpp_accepts_lowercase_preprocessor_source() {
    let src = write_program(
        "#define X 77\nprogram p\n  print *, X\nend program\n",
        "f90",
    );
    let out = unique_path("dash_cpp", "o");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-cpp",
            "-c",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "-cpp compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("-cpp is accepted for compatibility"),
        "expected a compatibility warning for -cpp: {}",
        stderr
    );
    assert!(out.exists(), "-cpp compile should produce an object file");
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn integer8_bit_intrinsics_accept_default_integer_positions() {
    let src = write_program(
        "program p\n  implicit none\n  integer(8) :: words(4) = 0_8\n  integer :: word_idx, bit_idx\n  word_idx = 1\n  bit_idx = 5\n  words(word_idx) = ior(words(word_idx), ishft(1_8, bit_idx))\n  print *, btest(words(word_idx), bit_idx)\nend program\n",
        "f90",
    );
    let out = unique_path("bit_intrinsics_i8", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "integer(8) bit intrinsic repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&out).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "integer(8) bit intrinsic repro should run:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains('T'),
        "expected btest result to stay true, got: {}",
        stdout
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn move_alloc_into_allocatable_component_of_class_dummy_compiles_and_runs() {
    let src = write_program(
        "module m\n  implicit none\n  type :: token_t\n    integer :: x = 0\n  end type\n  type :: list_t\n    type(token_t), allocatable :: tokens(:)\n  end type\ncontains\n  subroutine grow(this)\n    class(list_t), intent(inout) :: this\n    type(token_t), allocatable :: temp(:)\n    allocate(temp(4))\n    temp(3)%x = 42\n    call move_alloc(temp, this%tokens)\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(list_t) :: items\n  call grow(items)\n  print *, size(items%tokens), items%tokens(3)%x\nend program\n",
        "f90",
    );
    let out = unique_path("move_alloc_class_component", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "class-dummy MOVE_ALLOC repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&out).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "class-dummy MOVE_ALLOC repro should run:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("4") && stdout.contains("42"),
        "expected moved allocation to survive class-dummy component access, got: {}",
        stdout
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn move_alloc_between_scalar_allocatable_derived_components_compiles_and_runs() {
    let src = write_program(
        "module m\n  implicit none\n  type :: inner_t\n    integer :: x = 0\n  end type\n  type :: node_t\n    type(inner_t), allocatable :: val\n  end type\ncontains\n  subroutine grow(list)\n    type(node_t), allocatable, intent(inout) :: list(:)\n    type(node_t), allocatable :: tmp(:)\n    integer :: i\n    call move_alloc(list, tmp)\n    allocate(list(4))\n    do i = 1, min(size(tmp), 4)\n      if (allocated(tmp(i)%val)) then\n        call move_alloc(tmp(i)%val, list(i)%val)\n      end if\n    end do\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(node_t), allocatable :: list(:)\n  allocate(list(2))\n  allocate(list(1)%val)\n  list(1)%val%x = 99\n  call grow(list)\n  print *, size(list), allocated(list(1)%val), list(1)%val%x\nend program\n",
        "f90",
    );
    let out = unique_path("move_alloc_scalar_component", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "scalar-component MOVE_ALLOC repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&out).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "scalar-component MOVE_ALLOC repro should run:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("4") && stdout.contains('T') && stdout.contains("99"),
        "expected scalar allocatable component MOVE_ALLOC to preserve value, got: {}",
        stdout
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn move_alloc_between_scalar_allocatable_polymorphic_components_compiles() {
    let src = write_program(
        "module m\n  implicit none\n  type :: base_t\n  end type\n  type, extends(base_t) :: child_t\n    integer :: x = 0\n  end type\n  type :: node_t\n    class(base_t), allocatable :: val\n  end type\ncontains\n  subroutine grow(list)\n    type(node_t), allocatable, intent(inout) :: list(:)\n    type(node_t), allocatable :: tmp(:)\n    integer :: i\n    call move_alloc(list, tmp)\n    allocate(list(4))\n    do i = 1, min(size(tmp), 4)\n      if (allocated(tmp(i)%val)) then\n        call move_alloc(tmp(i)%val, list(i)%val)\n      end if\n    end do\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(node_t), allocatable :: list(:)\n  class(base_t), allocatable :: tmpval\n  allocate(list(1))\n  allocate(tmpval, source=child_t(77))\n  call move_alloc(tmpval, list(1)%val)\n  call grow(list)\n  print *, 0\nend program\n",
        "f90",
    );
    let out = unique_path("move_alloc_scalar_poly_component", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "scalar polymorphic component MOVE_ALLOC repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn move_alloc_from_scalar_allocatable_polymorphic_dummy_compiles() {
    let src = write_program(
        "module m\n  implicit none\n  type, abstract :: base_t\n  end type\n  type, extends(base_t) :: child_t\n    integer :: x = 0\n  end type\n  type :: node_t\n    class(base_t), allocatable :: val\n  end type\ncontains\n  subroutine take(val, node)\n    class(base_t), allocatable, intent(inout) :: val\n    type(node_t), intent(inout) :: node\n    call move_alloc(val, node%val)\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  class(base_t), allocatable :: tmp\n  type(node_t) :: node\n  allocate(tmp, source=child_t(17))\n  call take(tmp, node)\n  print *, 0\nend program\n",
        "f90",
    );
    let out = unique_path("move_alloc_scalar_poly_dummy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "scalar polymorphic allocatable dummy MOVE_ALLOC repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn move_alloc_scalar_class_allocatable_preserves_real_payload_through_select_type() {
    let src = write_program(
        "module m\n  implicit none\n  type, abstract :: generic_value\n  end type\n  type, extends(generic_value) :: float_value\n    real :: raw = -1.0\n  end type\n  type :: keyval_t\n    class(generic_value), allocatable :: val\n  contains\n    procedure :: set_float\n    procedure :: read_float\n  end type\ncontains\n  subroutine set_float(self, x)\n    class(keyval_t), intent(inout) :: self\n    real, intent(in) :: x\n    type(float_value), allocatable :: tmp\n    allocate(tmp)\n    tmp%raw = x\n    call move_alloc(tmp, self%val)\n  end subroutine\n  real function read_float(self) result(x)\n    class(keyval_t), intent(in) :: self\n    real, pointer :: ptr\n    ptr => cast_float(self%val)\n    if (.not.associated(ptr)) error stop 1\n    x = ptr\n  end function\n  function cast_float(val) result(ptr)\n    class(generic_value), intent(in), target :: val\n    real, pointer :: ptr\n    nullify(ptr)\n    select type(val)\n    type is(float_value)\n      ptr => val%raw\n    end select\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  type(keyval_t) :: kv\n  real :: x\n  call kv%set_float(1.0)\n  x = kv%read_float()\n  if (abs(x - 1.0) > 1.0e-6) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("move_alloc_scalar_class_real_payload", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("scalar class payload compile failed to spawn");
    assert!(
        compile.status.success(),
        "scalar class payload repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("scalar class payload run failed");
    assert!(
        run.status.success(),
        "scalar class payload repro should run: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "expected scalar class payload repro to preserve the float value, got: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn move_alloc_scalar_class_allocatable_preserves_real8_payload_through_select_type() {
    let src = write_program(
        "module m\n  implicit none\n  integer, parameter :: rk = selected_real_kind(15)\n  type, abstract :: generic_value\n  end type\n  type, extends(generic_value) :: float_value\n    real(rk) :: raw = -1.0_rk\n  end type\n  type :: keyval_t\n    class(generic_value), allocatable :: val\n  contains\n    procedure :: set_float\n    procedure :: read_float\n  end type\ncontains\n  subroutine set_float(self, x)\n    class(keyval_t), intent(inout) :: self\n    real(rk), intent(in) :: x\n    type(float_value), allocatable :: tmp\n    allocate(tmp)\n    tmp%raw = x\n    call move_alloc(tmp, self%val)\n  end subroutine\n  real(rk) function read_float(self) result(x)\n    class(keyval_t), intent(in) :: self\n    real(rk), pointer :: ptr\n    ptr => cast_float(self%val)\n    if (.not.associated(ptr)) error stop 1\n    x = ptr\n  end function\n  function cast_float(val) result(ptr)\n    class(generic_value), intent(in), target :: val\n    real(rk), pointer :: ptr\n    nullify(ptr)\n    select type(val)\n    type is(float_value)\n      ptr => val%raw\n    end select\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  type(keyval_t) :: kv\n  real(rk) :: x\n  call kv%set_float(1.0_rk)\n  x = kv%read_float()\n  if (abs(x - 1.0_rk) > 1.0e-12_rk) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("move_alloc_scalar_class_real8_payload", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("scalar class real8 payload compile failed to spawn");
    assert!(
        compile.status.success(),
        "scalar class real8 payload repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("scalar class real8 payload run failed");
    assert!(
        run.status.success(),
        "scalar class real8 payload repro should run: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "expected scalar class real8 payload repro to preserve the float value, got: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn real8_payload_round_trips_through_real4_getter_wrapper() {
    let src = write_program(
        "module m\n  implicit none\n  integer, parameter :: tf_sp = selected_real_kind(6)\n  integer, parameter :: rk = selected_real_kind(15)\n  type, abstract :: generic_value\n  end type\n  type, extends(generic_value) :: float_value\n    real(rk) :: raw = -1.0_rk\n  end type\n  type :: keyval_t\n    class(generic_value), allocatable :: val\n  contains\n    procedure :: set_float\n    procedure :: get_float_ptr\n    procedure :: get_float_sp\n  end type\ncontains\n  subroutine set_float(self, x)\n    class(keyval_t), intent(inout) :: self\n    real(rk), intent(in) :: x\n    type(float_value), allocatable :: tmp\n    allocate(tmp)\n    tmp%raw = x\n    call move_alloc(tmp, self%val)\n  end subroutine\n  subroutine get_float_ptr(self, ptr)\n    class(keyval_t), intent(in) :: self\n    real(rk), pointer, intent(out) :: ptr\n    ptr => cast_float(self%val)\n  end subroutine\n  subroutine get_float_sp(self, x)\n    class(keyval_t), intent(in) :: self\n    real(tf_sp), intent(out) :: x\n    real(rk), pointer :: ptr\n    call self%get_float_ptr(ptr)\n    if (.not.associated(ptr)) error stop 1\n    x = real(ptr, tf_sp)\n  end subroutine\n  function cast_float(val) result(ptr)\n    class(generic_value), intent(in), target :: val\n    real(rk), pointer :: ptr\n    nullify(ptr)\n    select type(val)\n    type is(float_value)\n      ptr => val%raw\n    end select\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  type(keyval_t) :: kv\n  real(tf_sp) :: x\n  call kv%set_float(1.0_rk)\n  call kv%get_float_sp(x)\n  if (abs(x - 1.0_tf_sp) > 1.0e-6_tf_sp) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("real8_to_real4_getter_wrapper", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("real8->real4 getter wrapper compile failed to spawn");
    assert!(
        compile.status.success(),
        "real8->real4 getter wrapper repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("real8->real4 getter wrapper run failed");
    assert!(
        run.status.success(),
        "real8->real4 getter wrapper repro should run: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "expected real8->real4 getter wrapper repro to preserve the value, got: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn move_alloc_nested_class_allocatable_container_preserves_real_payload() {
    let src = write_program(
        "module m\n  implicit none\n  type, abstract :: generic_value\n  end type\n  type, extends(generic_value) :: float_value\n    real :: raw = -1.0\n  end type\n  type, abstract :: value_t\n  end type\n  type, extends(value_t) :: keyval_t\n    class(generic_value), allocatable :: val\n  contains\n    procedure :: set_float\n    procedure :: read_float\n  end type\n  type :: node_t\n    class(value_t), allocatable :: val\n  end type\n  type :: list_t\n    type(node_t), allocatable :: lst(:)\n    integer :: n = 0\n  contains\n    procedure :: push_back\n    procedure :: get_float\n  end type\ncontains\n  subroutine set_float(self, x)\n    class(keyval_t), intent(inout) :: self\n    real, intent(in) :: x\n    type(float_value), allocatable :: tmp\n    allocate(tmp)\n    tmp%raw = x\n    call move_alloc(tmp, self%val)\n  end subroutine\n  real function read_float(self) result(x)\n    class(keyval_t), intent(in) :: self\n    real, pointer :: ptr\n    ptr => cast_float(self%val)\n    if (.not.associated(ptr)) error stop 1\n    x = ptr\n  end function\n  function cast_float(val) result(ptr)\n    class(generic_value), intent(in), target :: val\n    real, pointer :: ptr\n    nullify(ptr)\n    select type(val)\n    type is(float_value)\n      ptr => val%raw\n    end select\n  end function\n  subroutine push_back(self, val)\n    class(list_t), intent(inout) :: self\n    class(value_t), allocatable, intent(inout) :: val\n    if (.not.allocated(self%lst)) then\n      allocate(self%lst(1))\n    end if\n    self%n = self%n + 1\n    call move_alloc(val, self%lst(self%n)%val)\n  end subroutine\n  subroutine get_float(self, idx, x)\n    class(list_t), intent(inout) :: self\n    integer, intent(in) :: idx\n    real, intent(out) :: x\n    class(value_t), pointer :: any\n    nullify(any)\n    if (allocated(self%lst(idx)%val)) any => self%lst(idx)%val\n    if (.not.associated(any)) error stop 2\n    select type(any)\n    type is(keyval_t)\n      x = any%read_float()\n    class default\n      error stop 3\n    end select\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(list_t) :: list\n  type(keyval_t), allocatable :: item\n  real :: x\n  allocate(item)\n  call item%set_float(1.0)\n  call list%push_back(item)\n  call list%get_float(1, x)\n  if (abs(x - 1.0) > 1.0e-6) error stop 4\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("move_alloc_nested_class_real_payload", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("nested class payload compile failed to spawn");
    assert!(
        compile.status.success(),
        "nested class payload repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("nested class payload run failed");
    assert!(
        run.status.success(),
        "nested class payload repro should run: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "expected nested class payload repro to preserve the float value, got: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn alloc_source_from_class_pointer_deep_copies_nested_allocatable_payload() {
    let src = write_program(
        "module m\n  implicit none\n  type, abstract :: base_value\n  contains\n    procedure(destroy_i), deferred :: destroy\n  end type\n  abstract interface\n    subroutine destroy_i(self)\n      import :: base_value\n      class(base_value), intent(inout) :: self\n    end subroutine\n  end interface\n  type, abstract :: generic_value\n  end type\n  type, extends(generic_value) :: bool_value\n    logical :: raw = .false.\n  end type\n  type, extends(base_value) :: keyval_t\n    class(generic_value), allocatable :: val\n  contains\n    procedure :: destroy => keyval_destroy\n    procedure :: set_bool\n    procedure :: get_bool\n  end type\ncontains\n  subroutine set_bool(self, x)\n    class(keyval_t), intent(inout) :: self\n    logical, intent(in) :: x\n    type(bool_value), allocatable :: tmp\n    allocate(tmp)\n    tmp%raw = x\n    call move_alloc(tmp, self%val)\n  end subroutine\n  subroutine get_bool(self, x)\n    class(keyval_t), intent(in) :: self\n    logical, intent(out) :: x\n    logical, pointer :: ptr\n    ptr => cast_bool(self%val)\n    if (.not.associated(ptr)) error stop 1\n    x = ptr\n  end subroutine\n  function cast_bool(val) result(ptr)\n    class(generic_value), intent(in), target :: val\n    logical, pointer :: ptr\n    nullify(ptr)\n    select type(val)\n    type is(bool_value)\n      ptr => val%raw\n    end select\n  end function\n  subroutine keyval_destroy(self)\n    class(keyval_t), intent(inout) :: self\n    if (allocated(self%val)) deallocate(self%val)\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(keyval_t), allocatable, target :: src\n  class(base_value), allocatable :: clone\n  class(base_value), pointer :: ptr\n  logical :: x\n  allocate(src)\n  call src%set_bool(.false.)\n  ptr => src\n  allocate(clone, source=ptr)\n  call src%destroy()\n  deallocate(src)\n  select type(clone)\n  type is(keyval_t)\n    call clone%get_bool(x)\n  class default\n    error stop 2\n  end select\n  if (x .neqv. .false.) error stop 3\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("alloc_source_class_pointer_nested_payload", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("alloc-source nested payload compile failed to spawn");
    assert!(
        compile.status.success(),
        "alloc-source nested payload compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("alloc-source nested payload run failed");
    assert!(
        run.status.success(),
        "alloc-source nested payload run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected alloc-source nested payload output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn deferred_bound_get_through_class_holder_returns_allocatable_component_pointer() {
    let src = write_program(
        "module m\n  implicit none\n  type, abstract :: payload_t\n  end type\n  type, extends(payload_t) :: keyval_t\n    real :: x = 0.0\n  end type\n  type, abstract :: list_base_t\n  contains\n    procedure(get_i), deferred :: get\n    procedure(push_i), deferred :: push_back\n  end type\n  abstract interface\n    subroutine get_i(self, idx, ptr)\n      import :: list_base_t, payload_t\n      class(list_base_t), intent(inout), target :: self\n      integer, intent(in) :: idx\n      class(payload_t), pointer, intent(out) :: ptr\n    end subroutine\n    subroutine push_i(self, val)\n      import :: list_base_t, payload_t\n      class(list_base_t), intent(inout), target :: self\n      class(payload_t), allocatable, intent(inout) :: val\n    end subroutine\n  end interface\n  type :: node_t\n    class(payload_t), allocatable :: val\n  end type\n  type, extends(list_base_t) :: list_impl_t\n    type(node_t), allocatable :: lst(:)\n    integer :: n = 0\n  contains\n    procedure :: get => impl_get\n    procedure :: push_back => impl_push_back\n  end type\n  type :: array_t\n    class(list_base_t), allocatable :: list\n  contains\n    procedure :: init\n    procedure :: set_first\n    procedure :: get_first\n  end type\ncontains\n  subroutine init(self)\n    class(array_t), intent(out) :: self\n    type(list_impl_t), allocatable :: tmp\n    allocate(tmp)\n    call move_alloc(tmp, self%list)\n  end subroutine\n  subroutine impl_push_back(self, val)\n    class(list_impl_t), intent(inout), target :: self\n    class(payload_t), allocatable, intent(inout) :: val\n    if (.not. allocated(self%lst)) allocate(self%lst(1))\n    self%n = self%n + 1\n    call move_alloc(val, self%lst(self%n)%val)\n  end subroutine\n  subroutine impl_get(self, idx, ptr)\n    class(list_impl_t), intent(inout), target :: self\n    integer, intent(in) :: idx\n    class(payload_t), pointer, intent(out) :: ptr\n    nullify(ptr)\n    if (idx > 0 .and. idx <= self%n) then\n      if (allocated(self%lst(idx)%val)) ptr => self%lst(idx)%val\n    end if\n  end subroutine\n  subroutine set_first(self, x)\n    class(array_t), intent(inout) :: self\n    real, intent(in) :: x\n    type(keyval_t), allocatable :: tmp\n    allocate(tmp)\n    tmp%x = x\n    call self%list%push_back(tmp)\n  end subroutine\n  real function get_first(self) result(x)\n    class(array_t), intent(inout) :: self\n    class(payload_t), pointer :: ptr\n    x = -1.0\n    call self%list%get(1, ptr)\n    if (.not. associated(ptr)) error stop 1\n    select type(ptr)\n    type is(keyval_t)\n      x = ptr%x\n    class default\n      error stop 2\n    end select\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  type(array_t) :: arr\n  call arr%init()\n  call arr%set_first(1.0)\n  if (abs(arr%get_first() - 1.0) > 1.0e-6) error stop 3\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("deferred_bound_get_class_holder", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("deferred bound get class-holder compile failed to spawn");
    assert!(
        compile.status.success(),
        "deferred bound get class-holder repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("deferred bound get class-holder run failed");
    assert!(
        run.status.success(),
        "deferred bound get class-holder repro should run: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "expected deferred bound get class-holder repro to preserve the payload pointer, got: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_deferred_bound_get_through_class_holder_preserves_payload_pointer() {
    let dir = unique_dir("imported_deferred_bound_get_holder");
    let payload_src = write_program_in(
        &dir,
        "payload_m.f90",
        "module payload_m\n  implicit none\n  type, abstract :: payload_t\n  end type\n  type, extends(payload_t) :: keyval_t\n    real :: x = 0.0\n  end type\nend module\n",
    );
    let list_base_src = write_program_in(
        &dir,
        "list_base_m.f90",
        "module list_base_m\n  use payload_m, only : payload_t\n  implicit none\n  type, abstract :: list_base_t\n  contains\n    procedure(get_i), deferred :: get\n    procedure(push_i), deferred :: push_back\n  end type\n  abstract interface\n    subroutine get_i(self, idx, ptr)\n      import :: list_base_t, payload_t\n      class(list_base_t), intent(inout), target :: self\n      integer, intent(in) :: idx\n      class(payload_t), pointer, intent(out) :: ptr\n    end subroutine\n    subroutine push_i(self, val)\n      import :: list_base_t, payload_t\n      class(list_base_t), intent(inout), target :: self\n      class(payload_t), allocatable, intent(inout) :: val\n    end subroutine\n  end interface\nend module\n",
    );
    let list_impl_src = write_program_in(
        &dir,
        "list_impl_m.f90",
        "module list_impl_m\n  use payload_m, only : payload_t\n  use list_base_m, only : list_base_t\n  implicit none\n  type :: node_t\n    class(payload_t), allocatable :: val\n  end type\n  type, extends(list_base_t) :: list_impl_t\n    type(node_t), allocatable :: lst(:)\n    integer :: n = 0\n  contains\n    procedure :: get => impl_get\n    procedure :: push_back => impl_push_back\n  end type\ncontains\n  subroutine impl_push_back(self, val)\n    class(list_impl_t), intent(inout), target :: self\n    class(payload_t), allocatable, intent(inout) :: val\n    if (.not. allocated(self%lst)) allocate(self%lst(1))\n    self%n = self%n + 1\n    call move_alloc(val, self%lst(self%n)%val)\n  end subroutine\n  subroutine impl_get(self, idx, ptr)\n    class(list_impl_t), intent(inout), target :: self\n    integer, intent(in) :: idx\n    class(payload_t), pointer, intent(out) :: ptr\n    nullify(ptr)\n    if (idx > 0 .and. idx <= self%n) then\n      if (allocated(self%lst(idx)%val)) ptr => self%lst(idx)%val\n    end if\n  end subroutine\nend module\n",
    );
    let holder_src = write_program_in(
        &dir,
        "holder_m.f90",
        "module holder_m\n  use payload_m, only : payload_t, keyval_t\n  use list_base_m, only : list_base_t\n  use list_impl_m, only : list_impl_t\n  implicit none\n  type :: array_t\n    class(list_base_t), allocatable :: list\n  contains\n    procedure :: init\n    procedure :: set_first\n    procedure :: get_first\n  end type\ncontains\n  subroutine init(self)\n    class(array_t), intent(out) :: self\n    type(list_impl_t), allocatable :: tmp\n    allocate(tmp)\n    call move_alloc(tmp, self%list)\n  end subroutine\n  subroutine set_first(self, x)\n    class(array_t), intent(inout) :: self\n    real, intent(in) :: x\n    type(keyval_t), allocatable :: tmp\n    allocate(tmp)\n    tmp%x = x\n    call self%list%push_back(tmp)\n  end subroutine\n  real function get_first(self) result(x)\n    class(array_t), intent(inout) :: self\n    class(payload_t), pointer :: ptr\n    x = -1.0\n    call self%list%get(1, ptr)\n    if (.not. associated(ptr)) error stop 1\n    select type(ptr)\n    type is(keyval_t)\n      x = ptr%x\n    class default\n      error stop 2\n    end select\n  end function\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use holder_m, only : array_t\n  implicit none\n  type(array_t) :: arr\n  call arr%init()\n  call arr%set_first(1.0)\n  if (abs(arr%get_first() - 1.0) > 1.0e-6) error stop 3\n  print *, 'ok'\nend program\n",
    );

    let payload_obj = dir.join("payload_m.o");
    let list_base_obj = dir.join("list_base_m.o");
    let list_impl_obj = dir.join("list_impl_m.o");
    let holder_obj = dir.join("holder_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("imported_deferred_bound_get_holder.bin");

    for (src, obj, needs_i) in [
        (&payload_src, &payload_obj, false),
        (&list_base_src, &list_base_obj, true),
        (&list_impl_src, &list_impl_obj, true),
        (&holder_src, &holder_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "imported deferred bound get compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            payload_obj.to_str().unwrap(),
            list_base_obj.to_str().unwrap(),
            list_impl_obj.to_str().unwrap(),
            holder_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "imported deferred bound get link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run failed");
    assert!(
        run.status.success(),
        "imported deferred bound get runtime failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "expected imported deferred bound get to preserve the payload pointer, got: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_keyval_cast_after_deferred_bound_get_preserves_real8_payload() {
    let dir = unique_dir("imported_keyval_cast_after_get");
    let value_src = write_program_in(
        &dir,
        "value_m.f90",
        "module value_m\n  implicit none\n  integer, parameter :: tf_sp = selected_real_kind(6)\n  integer, parameter :: rk = selected_real_kind(15)\n  type, abstract :: inner_value_t\n  end type\n  type, extends(inner_value_t) :: float_value_t\n    real(rk) :: raw = -1.0_rk\n  end type\n  type, abstract :: value_t\n  end type\n  type, extends(value_t) :: keyval_t\n    class(inner_value_t), allocatable :: val\n  contains\n    procedure :: set_float\n    procedure :: get_float_ptr\n    procedure :: get_float_sp\n  end type\n  interface cast_to_keyval\n    module procedure :: cast_to_keyval_impl\n  end interface\ncontains\n  subroutine set_float(self, x)\n    class(keyval_t), intent(inout) :: self\n    real(rk), intent(in) :: x\n    type(float_value_t), allocatable :: tmp\n    allocate(tmp)\n    tmp%raw = x\n    call move_alloc(tmp, self%val)\n  end subroutine\n  subroutine get_float_ptr(self, ptr)\n    class(keyval_t), intent(in) :: self\n    real(rk), pointer, intent(out) :: ptr\n    ptr => cast_float(self%val)\n  end subroutine\n  subroutine get_float_sp(self, x)\n    class(keyval_t), intent(in) :: self\n    real(tf_sp), intent(out) :: x\n    real(rk), pointer :: ptr\n    call self%get_float_ptr(ptr)\n    if (.not.associated(ptr)) error stop 1\n    x = real(ptr, tf_sp)\n  end subroutine\n  function cast_float(val) result(ptr)\n    class(inner_value_t), intent(in), target :: val\n    real(rk), pointer :: ptr\n    nullify(ptr)\n    select type(val)\n    type is(float_value_t)\n      ptr => val%raw\n    end select\n  end function\n  function cast_to_keyval_impl(ptr) result(kval)\n    class(value_t), intent(in), target :: ptr\n    type(keyval_t), pointer :: kval\n    nullify(kval)\n    select type(ptr)\n    type is(keyval_t)\n      kval => ptr\n    end select\n  end function\nend module\n",
    );
    let list_base_src = write_program_in(
        &dir,
        "list_base_m.f90",
        "module list_base_m\n  use value_m, only : value_t\n  implicit none\n  type, abstract :: list_base_t\n  contains\n    procedure(get_i), deferred :: get\n    procedure(push_i), deferred :: push_back\n  end type\n  abstract interface\n    subroutine get_i(self, idx, ptr)\n      import :: list_base_t, value_t\n      class(list_base_t), intent(inout), target :: self\n      integer, intent(in) :: idx\n      class(value_t), pointer, intent(out) :: ptr\n    end subroutine\n    subroutine push_i(self, val)\n      import :: list_base_t, value_t\n      class(list_base_t), intent(inout), target :: self\n      class(value_t), allocatable, intent(inout) :: val\n    end subroutine\n  end interface\nend module\n",
    );
    let list_impl_src = write_program_in(
        &dir,
        "list_impl_m.f90",
        "module list_impl_m\n  use value_m, only : value_t\n  use list_base_m, only : list_base_t\n  implicit none\n  type :: node_t\n    class(value_t), allocatable :: val\n  end type\n  type, extends(list_base_t) :: list_impl_t\n    type(node_t), allocatable :: lst(:)\n    integer :: n = 0\n  contains\n    procedure :: get => impl_get\n    procedure :: push_back => impl_push_back\n  end type\ncontains\n  subroutine impl_push_back(self, val)\n    class(list_impl_t), intent(inout), target :: self\n    class(value_t), allocatable, intent(inout) :: val\n    if (.not. allocated(self%lst)) allocate(self%lst(1))\n    self%n = self%n + 1\n    call move_alloc(val, self%lst(self%n)%val)\n  end subroutine\n  subroutine impl_get(self, idx, ptr)\n    class(list_impl_t), intent(inout), target :: self\n    integer, intent(in) :: idx\n    class(value_t), pointer, intent(out) :: ptr\n    nullify(ptr)\n    if (idx > 0 .and. idx <= self%n) then\n      if (allocated(self%lst(idx)%val)) ptr => self%lst(idx)%val\n    end if\n  end subroutine\nend module\n",
    );
    let holder_src = write_program_in(
        &dir,
        "holder_m.f90",
        "module holder_m\n  use value_m, only : tf_sp, rk, value_t, keyval_t, cast_to_keyval\n  use list_base_m, only : list_base_t\n  use list_impl_m, only : list_impl_t\n  implicit none\n  type :: array_t\n    class(list_base_t), allocatable :: list\n  contains\n    procedure :: init\n    procedure :: set_first\n    procedure :: get_first\n  end type\ncontains\n  subroutine init(self)\n    class(array_t), intent(out) :: self\n    type(list_impl_t), allocatable :: tmp\n    allocate(tmp)\n    call move_alloc(tmp, self%list)\n  end subroutine\n  subroutine set_first(self, x)\n    class(array_t), intent(inout) :: self\n    real(rk), intent(in) :: x\n    type(keyval_t), allocatable :: tmp\n    allocate(tmp)\n    call tmp%set_float(x)\n    call self%list%push_back(tmp)\n  end subroutine\n  real(tf_sp) function get_first(self) result(x)\n    class(array_t), intent(inout) :: self\n    class(value_t), pointer :: tmp\n    type(keyval_t), pointer :: ptr\n    x = -1.0_tf_sp\n    call self%list%get(1, tmp)\n    if (.not.associated(tmp)) error stop 2\n    ptr => cast_to_keyval(tmp)\n    if (.not.associated(ptr)) error stop 3\n    call ptr%get_float_sp(x)\n  end function\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use holder_m, only : array_t\n  use value_m, only : rk, tf_sp\n  implicit none\n  type(array_t) :: arr\n  real(tf_sp) :: x\n  call arr%init()\n  call arr%set_first(1.0_rk)\n  x = arr%get_first()\n  if (abs(x - 1.0_tf_sp) > 1.0e-6_tf_sp) error stop 4\n  print *, 'ok'\nend program\n",
    );

    let value_obj = dir.join("value_m.o");
    let list_base_obj = dir.join("list_base_m.o");
    let list_impl_obj = dir.join("list_impl_m.o");
    let holder_obj = dir.join("holder_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("imported_keyval_cast_after_get.bin");

    for (src, obj, needs_i) in [
        (&value_src, &value_obj, false),
        (&list_base_src, &list_base_obj, true),
        (&list_impl_src, &list_impl_obj, true),
        (&holder_src, &holder_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "imported keyval cast-after-get compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            value_obj.to_str().unwrap(),
            list_base_obj.to_str().unwrap(),
            list_impl_obj.to_str().unwrap(),
            holder_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "imported keyval cast-after-get link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run failed");
    assert!(
        run.status.success(),
        "imported keyval cast-after-get runtime failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "expected imported keyval cast-after-get to preserve the value, got: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_generic_function_accepts_imported_derived_actual() {
    let dir = unique_dir("imported_generic_function_derived");
    let types_src = write_program_in(
        &dir,
        "types_m.f90",
        "module types_m\n  implicit none\n  type :: date_t\n    integer :: year = 0\n  end type\nend module\n",
    );
    let utils_src = write_program_in(
        &dir,
        "utils_m.f90",
        "module utils_m\n  use types_m, only : date_t\n  implicit none\n  interface to_string\n    module procedure :: to_string_date\n  end interface\ncontains\n  function to_string_date(val) result(str)\n    type(date_t), intent(in) :: val\n    character(len=:), allocatable :: str\n    if (val%year == 1987) then\n      str = 'ok'\n    else\n      str = 'bad'\n    end if\n  end function\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use types_m, only : date_t\n  use utils_m, only : to_string\n  implicit none\n  type(date_t) :: ref\n  ref%year = 1987\n  if (to_string(ref) /= 'ok') error stop 1\n  print *, 'ok'\nend program\n",
    );

    let types_obj = dir.join("types_m.o");
    let utils_obj = dir.join("utils_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("imported_generic_function_derived.bin");

    for (src, obj, needs_i) in [
        (&types_src, &types_obj, false),
        (&utils_src, &utils_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "imported generic function compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            types_obj.to_str().unwrap(),
            utils_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "imported generic function link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run failed");
    assert!(
        run.status.success(),
        "imported generic function runtime failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "expected imported generic function to accept the derived actual, got: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merged_imported_generic_function_preserves_imported_derived_specifics() {
    let dir = unique_dir("merged_imported_generic_function");
    let types_src = write_program_in(
        &dir,
        "types_m.f90",
        "module types_m\n  implicit none\n  type :: date_t\n    integer :: year = 0\n  end type\nend module\n",
    );
    let date_src = write_program_in(
        &dir,
        "date_utils_m.f90",
        "module date_utils_m\n  use types_m, only : date_t\n  implicit none\n  interface to_string\n    module procedure :: to_string_date\n  end interface\ncontains\n  function to_string_date(val) result(str)\n    type(date_t), intent(in) :: val\n    character(len=:), allocatable :: str\n    if (val%year == 1987) then\n      str = 'date'\n    else\n      str = 'bad'\n    end if\n  end function\nend module\n",
    );
    let merged_src = write_program_in(
        &dir,
        "merged_utils_m.f90",
        "module merged_utils_m\n  use date_utils_m, only : to_string\n  implicit none\n  interface to_string\n    module procedure :: to_string_i4\n  end interface\ncontains\n  function to_string_i4(val) result(str)\n    integer, intent(in) :: val\n    character(len=:), allocatable :: str\n    if (val == 7) then\n      str = 'int'\n    else\n      str = 'bad'\n    end if\n  end function\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use types_m, only : date_t\n  use merged_utils_m, only : to_string\n  implicit none\n  type(date_t) :: ref\n  ref%year = 1987\n  if (to_string(ref) /= 'date') error stop 1\n  if (to_string(7) /= 'int') error stop 2\n  print *, 'ok'\nend program\n",
    );

    let types_obj = dir.join("types_m.o");
    let date_obj = dir.join("date_utils_m.o");
    let merged_obj = dir.join("merged_utils_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("merged_imported_generic_function.bin");

    for (src, obj, needs_i) in [
        (&types_src, &types_obj, false),
        (&date_src, &date_obj, true),
        (&merged_src, &merged_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "merged imported generic function compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            types_obj.to_str().unwrap(),
            date_obj.to_str().unwrap(),
            merged_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "merged imported generic function link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run failed");
    assert!(
        run.status.success(),
        "merged imported generic function runtime failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "expected merged imported generic function to keep imported specifics, got: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merged_imported_generic_amod_reexports_imported_specifics() {
    let dir = unique_dir("merged_imported_generic_amod");
    let types_src = write_program_in(
        &dir,
        "types_m.f90",
        "module types_m\n  implicit none\n  type :: date_t\n    integer :: year = 0\n  end type\nend module\n",
    );
    let date_src = write_program_in(
        &dir,
        "date_utils_m.f90",
        "module date_utils_m\n  use types_m, only : date_t\n  implicit none\n  interface to_string\n    module procedure :: to_string_date\n  end interface\ncontains\n  function to_string_date(val) result(str)\n    type(date_t), intent(in) :: val\n    character(len=:), allocatable :: str\n    if (val%year == 1987) then\n      str = 'date'\n    else\n      str = 'bad'\n    end if\n  end function\nend module\n",
    );
    let merged_src = write_program_in(
        &dir,
        "merged_utils_m.f90",
        "module merged_utils_m\n  use date_utils_m, only : to_string\n  implicit none\n  interface to_string\n    module procedure :: to_string_i4\n  end interface\ncontains\n  function to_string_i4(val) result(str)\n    integer, intent(in) :: val\n    character(len=:), allocatable :: str\n    if (val == 7) then\n      str = 'int'\n    else\n      str = 'bad'\n    end if\n  end function\nend module\n",
    );

    for (src, obj, needs_i) in [
        (&types_src, dir.join("types_m.o"), false),
        (&date_src, dir.join("date_utils_m.o"), true),
        (&merged_src, dir.join("merged_utils_m.o"), true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "merged imported generic amod compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let amod_text = std::fs::read_to_string(dir.join("merged_utils_m.amod"))
        .expect("missing merged_utils_m.amod");
    assert!(
        amod_text.contains("@interface to_string"),
        "expected merged generic interface in amod, got: {}",
        amod_text
    );
    assert!(
        amod_text.contains("@specific to_string_date"),
        "expected imported specific to be preserved in merged amod, got: {}",
        amod_text
    );
    assert!(
        amod_text.contains("@specific to_string_i4"),
        "expected local specific to remain in merged amod, got: {}",
        amod_text
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn type_bound_subroutine_call_uses_module_qualified_symbol_and_links() {
    let src = write_program(
        "module m\n  implicit none\n  type :: counter_t\n    integer :: value = 0\n  contains\n    procedure :: bump => counter_bump\n  end type\ncontains\n  subroutine counter_bump(this, delta)\n    class(counter_t), intent(inout) :: this\n    integer, intent(in) :: delta\n    this%value = this%value + delta\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(counter_t) :: counter\n  call counter%bump(7)\n  print *, counter%value\nend program\n",
        "f90",
    );
    let out = unique_path("type_bound_subroutine_link", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "type-bound subroutine repro should compile and link: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&out).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "type-bound subroutine repro should run:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains('7'),
        "expected type-bound subroutine call to mutate the receiver, got: {}",
        stdout
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn type_bound_call_preserves_absent_optional_slots() {
    let src = write_program(
        "module m\n  implicit none\n  type :: list_t\n    integer :: x = 0\n  contains\n    procedure :: init\n    procedure :: ensure\n  end type\ncontains\n  subroutine init(this, n)\n    class(list_t), intent(inout) :: this\n    integer, intent(in), optional :: n\n    if (present(n)) then\n      this%x = n\n    else\n      this%x = 42\n    end if\n  end subroutine\n\n  subroutine ensure(this)\n    class(list_t), intent(inout) :: this\n    call this%init()\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(list_t) :: v\n  call v%ensure()\n  if (v%x /= 42) error stop 1\n  print *, v%x\nend program\n",
        "f90",
    );
    let out = unique_path("type_bound_optional_absent", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "type-bound optional repro should compile and link: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&out).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "type-bound optional repro should run:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("42"),
        "expected absent optional on type-bound call to arrive as not-present, got: {}",
        stdout
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_type_bound_call_preserves_absent_optional_slots() {
    let dir = unique_dir("imported_type_bound_optional");
    let mod_src = dir.join("m.f90");
    let main_src = dir.join("p.f90");
    std::fs::write(
        &mod_src,
        "module m\n  implicit none\n  type :: list_t\n    integer :: x = 0\n  contains\n    procedure :: init\n  end type\ncontains\n  subroutine init(this, n)\n    class(list_t), intent(inout) :: this\n    integer, intent(in), optional :: n\n    if (present(n)) then\n      this%x = n\n    else\n      this%x = 42\n    end if\n  end subroutine\nend module\n",
    )
    .expect("write module");
    std::fs::write(
        &main_src,
        "program p\n  use m\n  implicit none\n  type(list_t) :: v\n  call v%init()\n  if (v%x /= 42) error stop 1\n  print *, v%x\nend program\n",
    )
    .expect("write program");

    let mod_obj = dir.join("m.o");
    let module_build = Command::new(compiler("armfortas"))
        .args([
            "-c",
            mod_src.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        module_build.status.success(),
        "module build should succeed: {}",
        String::from_utf8_lossy(&module_build.stderr)
    );

    let main_obj = dir.join("p.o");
    let main_compile = Command::new(compiler("armfortas"))
        .args([
            "-c",
            main_src.to_str().unwrap(),
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        main_compile.status.success(),
        "main compile should succeed: {}",
        String::from_utf8_lossy(&main_compile.stderr)
    );

    let out = dir.join("p.bin");
    let link = Command::new(compiler("armfortas"))
        .args([
            main_obj.to_str().unwrap(),
            mod_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        link.status.success(),
        "link should succeed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "imported type-bound optional repro should run:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("42"),
        "expected imported absent optional on type-bound call to arrive as not-present, got: {}",
        stdout
    );
}

#[test]
fn imported_type_bound_alias_preserves_absent_optional_slots() {
    let dir = unique_dir("imported_type_bound_optional_alias");
    let mod_src = dir.join("m.f90");
    let main_src = dir.join("p.f90");
    std::fs::write(
        &mod_src,
        "module m\n  implicit none\n  type :: list_t\n    integer :: x = 0\n  contains\n    procedure :: init => token_list_init\n  end type\ncontains\n  subroutine token_list_init(this, n)\n    class(list_t), intent(inout) :: this\n    integer, intent(in), optional :: n\n    if (present(n)) then\n      this%x = n\n    else\n      this%x = 42\n    end if\n  end subroutine\nend module\n",
    )
    .expect("write module");
    std::fs::write(
        &main_src,
        "program p\n  use m\n  implicit none\n  type(list_t) :: v\n  call v%init()\n  if (v%x /= 42) error stop 1\n  print *, v%x\nend program\n",
    )
    .expect("write program");

    let mod_obj = dir.join("m.o");
    let module_build = Command::new(compiler("armfortas"))
        .args([
            "-c",
            mod_src.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        module_build.status.success(),
        "module build should succeed: {}",
        String::from_utf8_lossy(&module_build.stderr)
    );

    let main_obj = dir.join("p.o");
    let main_compile = Command::new(compiler("armfortas"))
        .args([
            "-c",
            main_src.to_str().unwrap(),
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        main_compile.status.success(),
        "main compile should succeed: {}",
        String::from_utf8_lossy(&main_compile.stderr)
    );

    let out = dir.join("p.bin");
    let link = Command::new(compiler("armfortas"))
        .args([
            main_obj.to_str().unwrap(),
            mod_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        link.status.success(),
        "link should succeed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "imported aliased type-bound optional repro should run:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("42"),
        "expected imported aliased absent optional on type-bound call to arrive as not-present, got: {}",
        stdout
    );
}

#[test]
fn type_bound_call_on_allocatable_component_array_element_mutates_real_receiver() {
    let src = write_program(
        "module m\n  implicit none\n  type :: item_t\n    integer :: n = 0\n    integer, allocatable :: vals(:)\n  contains\n    procedure :: push\n  end type\n  type :: container_t\n    type(item_t), allocatable :: items(:)\n  contains\n    procedure :: init\n  end type\ncontains\n  subroutine push(this, value)\n    class(item_t), intent(inout) :: this\n    integer, intent(in) :: value\n    if (.not. allocated(this%vals)) allocate(this%vals(4))\n    this%n = this%n + 1\n    this%vals(this%n) = value\n  end subroutine\n  subroutine init(this, count)\n    class(container_t), intent(inout) :: this\n    integer, intent(in) :: count\n    if (allocated(this%items)) deallocate(this%items)\n    allocate(this%items(count))\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(container_t) :: box\n  call box%init(2)\n  call box%items(1)%push(19)\n  if (box%items(1)%n /= 1) error stop 1\n  if (box%items(1)%vals(1) /= 19) error stop 2\n  print *, box%items(1)%n, box%items(1)%vals(1)\nend program\n",
        "f90",
    );
    let out = unique_path("type_bound_component_array_elem", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "component array element type-bound repro should compile and link: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&out).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "component array element type-bound repro should run:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("1") && stdout.contains("19"),
        "expected component array element type-bound call to mutate the real receiver, got: {}",
        stdout
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
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
fn ffree_line_length_none_is_accepted_with_warning() {
    let src = write_program("program p\n  print *, 7\nend program\n", "f90");
    let out = unique_path("ffree_line_length_none", "o");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            "-ffree-line-length-none",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(result.status.success(), "flag should be accepted");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("-ffree-line-length-none is accepted for compatibility"),
        "expected compatibility warning: {}",
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
fn dash_j_also_searches_for_modules_on_dependent_compile() {
    let dir = unique_dir("dashj_search");
    let mod_dir = dir.join("modules");
    std::fs::create_dir_all(&mod_dir).unwrap();
    let mod_src = write_program_in(
        &dir,
        "producer.f90",
        "module producer_mod\n  implicit none\n  integer :: x = 42\nend module\n",
    );
    let user_src = write_program_in(
        &dir,
        "consumer.f90",
        "program p\n  use producer_mod\n  implicit none\n  if (x /= 42) error stop 1\nend program\n",
    );
    let mod_obj = dir.join("producer.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .args([
            "-c",
            "-J",
            mod_dir.to_str().unwrap(),
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
    assert!(
        mod_dir.join("producer_mod.amod").exists(),
        "producer compile should emit .amod to -J dir"
    );

    let user_obj = dir.join("consumer.o");
    let compile_user = Command::new(compiler("armfortas"))
        .args([
            "-c",
            "-J",
            mod_dir.to_str().unwrap(),
            user_src.to_str().unwrap(),
            "-o",
            user_obj.to_str().unwrap(),
        ])
        .output()
        .expect("consumer compile spawn failed");
    assert!(
        compile_user.status.success(),
        "-J should also search that directory for modules: {}",
        String::from_utf8_lossy(&compile_user.stderr)
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
fn deferred_type_bound_proc_interface_survives_amod_export() {
    let dir = unique_dir("deferred_tbp_interface_amod");
    let mod_src = write_program_in(
        &dir,
        "m.f90",
        "module m\n  implicit none\n  type, abstract, public :: list_t\n  contains\n    procedure(push_iface), deferred :: push\n  end type\n  abstract interface\n    subroutine push_iface(self)\n      import :: list_t\n      class(list_t), intent(inout) :: self\n    end subroutine\n  end interface\nend module\n",
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
    let amod_text = std::fs::read_to_string(&amod).expect("missing m.amod");
    assert!(
        amod_text.contains("@binds push"),
        "deferred type-bound procedure binding should survive into .amod: {}",
        amod_text
    );
    assert!(
        !amod_text.contains("@binds ("),
        "interface-spec type-bound procedure should not degrade into '(' in .amod: {}",
        amod_text
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn polymorphic_component_type_bound_call_dispatches_through_scalar_allocatable_descriptor() {
    let src = write_program(
        "module m\n  implicit none\n  type, abstract :: list_base_t\n  contains\n    procedure(get_len_i), deferred :: get_len\n    procedure(push_i), deferred :: push\n  end type\n  abstract interface\n    integer function get_len_i(self) result(length)\n      import :: list_base_t\n      class(list_base_t), intent(in) :: self\n    end function\n    subroutine push_i(self, n)\n      import :: list_base_t\n      class(list_base_t), intent(inout) :: self\n      integer, intent(in) :: n\n    end subroutine\n  end interface\n  type, extends(list_base_t) :: list_impl_t\n    integer :: n = 0\n  contains\n    procedure :: get_len => impl_get_len\n    procedure :: push => impl_push\n  end type\n  type :: array_t\n    class(list_base_t), allocatable :: list\n  contains\n    procedure :: init => array_init\n    procedure :: size_of => array_size_of\n    procedure :: push_one => array_push_one\n  end type\ncontains\n  subroutine array_init(self, n)\n    class(array_t), intent(out) :: self\n    integer, intent(in) :: n\n    type(list_impl_t), allocatable :: list\n    allocate(list)\n    list%n = n\n    call move_alloc(list, self%list)\n  end subroutine\n  integer function impl_get_len(self) result(length)\n    class(list_impl_t), intent(in) :: self\n    length = self%n\n  end function\n  subroutine impl_push(self, n)\n    class(list_impl_t), intent(inout) :: self\n    integer, intent(in) :: n\n    self%n = self%n + n\n  end subroutine\n  integer function array_size_of(self) result(length)\n    class(array_t), intent(in) :: self\n    length = self%list%get_len()\n  end function\n  subroutine array_push_one(self, n)\n    class(array_t), intent(inout) :: self\n    integer, intent(in) :: n\n    call self%list%push(n)\n  end subroutine\nend module\nprogram main\n  use m\n  implicit none\n  type(array_t) :: arr\n  call arr%init(3)\n  call arr%push_one(4)\n  if (arr%size_of() /= 7) error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("polymorphic_component_tbp_dispatch", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("compile spawn failed");
    assert!(
        compile.status.success(),
        "polymorphic component bound-call dispatch program should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "polymorphic component bound-call dispatch runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "expected polymorphic component bound-call dispatch to reach concrete implementation, got: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn typed_allocate_class_component_sets_runtime_type_tag_for_dispatch() {
    let src = write_program(
        "module m\n  implicit none\n  type, abstract :: base\n  contains\n    procedure(get_keys_i), deferred :: get_keys\n  end type\n  abstract interface\n    subroutine get_keys_i(self)\n      import :: base\n      class(base), intent(inout) :: self\n    end subroutine\n  end interface\n  type, extends(base) :: child\n  contains\n    procedure :: get_keys => child_get_keys\n  end type\n  type :: holder\n    class(base), allocatable :: map\n  contains\n    procedure :: ping\n  end type\ncontains\n  subroutine child_get_keys(self)\n    class(child), intent(inout) :: self\n    print *, 'keys'\n  end subroutine\n  subroutine ping(self)\n    class(holder), intent(inout) :: self\n    call self%map%get_keys()\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(holder) :: h\n  allocate(child :: h%map)\n  call h%ping()\nend program\n",
        "f90",
    );
    let out = unique_path("typed_allocate_class_component_dispatch", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("typed allocate class component dispatch compile failed to spawn");
    assert!(
        compile.status.success(),
        "typed allocate class component dispatch should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("typed allocate class component dispatch run failed");
    assert!(
        run.status.success(),
        "typed allocate class component dispatch should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("keys"),
        "unexpected typed allocate class component dispatch output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn component_bound_dispatch_keeps_module_specific_target_name() {
    let src = write_program(
        "module list_m\n  implicit none\n  type, abstract :: list_t\n  contains\n    procedure(get_len_i), deferred :: get_len\n  end type\n  abstract interface\n    integer function get_len_i(self) result(length)\n      import :: list_t\n      class(list_t), intent(in) :: self\n    end function\n  end interface\n  type, extends(list_t) :: list_impl_t\n    integer :: n = 0\n  contains\n    procedure :: get_len => impl_get_len\n  end type\ncontains\n  integer function impl_get_len(self) result(length)\n    class(list_impl_t), intent(in) :: self\n    length = self%n\n  end function\nend module\n\nmodule array_m\n  use list_m, only : list_t, list_impl_t\n  implicit none\n  type :: array_t\n    class(list_t), allocatable :: list\n  contains\n    procedure :: init\n    procedure :: get_len => array_get_len\n  end type\ncontains\n  subroutine init(self, n)\n    class(array_t), intent(out) :: self\n    integer, intent(in) :: n\n    type(list_impl_t), allocatable :: tmp\n    allocate(tmp)\n    tmp%n = n\n    call move_alloc(tmp, self%list)\n  end subroutine\n  integer function array_get_len(self) result(length)\n    class(array_t), intent(in) :: self\n    length = self%list%get_len()\n  end function\nend module\n\nprogram p\n  use array_m\n  implicit none\n  type(array_t) :: arr\n  call arr%init(7)\n  if (arr%get_len() /= 7) error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("module_specific_bound_target", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("module-specific bound target compile failed to spawn");
    assert!(
        compile.status.success(),
        "module-specific bound target should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("module-specific bound target run failed");
    assert!(
        run.status.success(),
        "module-specific bound target should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected module-specific bound target output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn inherited_generic_tbp_alias_retargets_to_cross_tu_override() {
    let dir = unique_dir("generic_tbp_alias_override");
    let base_src = write_program_in(
        &dir,
        "base_m.f90",
        "module base_m\n  implicit none\n  type, abstract :: base_t\n  contains\n    generic :: ping => ping_int\n    procedure(ping_int), deferred :: ping_int\n  end type\n  abstract interface\n    subroutine ping_int(self, out)\n      import :: base_t\n      class(base_t), intent(inout) :: self\n      integer, intent(out) :: out\n    end subroutine\n  end interface\nend module\n",
    );
    let impl_src = write_program_in(
        &dir,
        "impl_m.f90",
        "module impl_m\n  use base_m, only : base_t\n  implicit none\n  type, extends(base_t) :: child_t\n  contains\n    procedure :: ping_int => child_ping\n  end type\ncontains\n  subroutine child_ping(self, out)\n    class(child_t), intent(inout) :: self\n    integer, intent(out) :: out\n    out = 17\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use impl_m, only : child_t\n  implicit none\n  type(child_t) :: x\n  integer :: out\n  call x%ping(out)\n  if (out /= 17) error stop 1\n  print *, 'ok'\nend program\n",
    );

    let base_obj = dir.join("base_m.o");
    let impl_obj = dir.join("impl_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("generic_tbp_alias_override.bin");

    for (src, obj, needs_i) in [
        (&base_src, &base_obj, false),
        (&impl_src, &impl_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "generic tbp alias override compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            base_obj.to_str().unwrap(),
            impl_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("generic tbp alias override link spawn failed");
    assert!(
        link.status.success(),
        "generic tbp alias override should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("generic tbp alias override run failed");
    assert!(
        run.status.success(),
        "generic tbp alias override should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected generic tbp alias override output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_inherited_bound_proc_dispatches_from_external_base_module() {
    let dir = unique_dir("imported_inherited_bound_proc_dispatch");
    let base_src = write_program_in(
        &dir,
        "base_m.f90",
        "module base_m\n  implicit none\n  type, abstract :: base_t\n  contains\n    procedure :: match => base_match\n  end type\ncontains\n  logical function base_match(self, n) result(ok)\n    class(base_t), intent(in) :: self\n    integer, intent(in) :: n\n    ok = n == 17\n  end function\nend module\n",
    );
    let child_src = write_program_in(
        &dir,
        "child_m.f90",
        "module child_m\n  use base_m, only : base_t\n  implicit none\n  type, extends(base_t) :: child_t\n  end type\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use base_m, only : base_t\n  use child_m, only : child_t\n  implicit none\n  class(base_t), allocatable :: x\n  allocate(child_t :: x)\n  if (.not. x%match(17)) error stop 1\n  print *, 'ok'\nend program\n",
    );

    let base_obj = dir.join("base_m.o");
    let child_obj = dir.join("child_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("imported_inherited_bound_proc_dispatch.bin");

    for (src, obj, needs_i) in [
        (&base_src, &base_obj, false),
        (&child_src, &child_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "imported inherited bound proc compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            base_obj.to_str().unwrap(),
            child_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "imported inherited bound proc link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run failed");
    assert!(
        run.status.success(),
        "imported inherited bound proc runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected imported inherited bound proc output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merged_generic_prefers_imported_pointer_specific_over_local_array_specific() {
    let dir = unique_dir("merged_generic_pointer_specific");
    let keyval_src = write_program_in(
        &dir,
        "keyval_m.f90",
        "module keyval_m\n  implicit none\n  type :: keyval_t\n    real :: value = -1.0\n  end type\n  interface set_value\n    module procedure :: set_value_float_sp\n  end interface\ncontains\n  subroutine set_value_float_sp(ptr, val, stat, origin)\n    type(keyval_t), pointer, intent(inout) :: ptr\n    real, intent(in) :: val\n    integer, intent(out), optional :: stat\n    integer, intent(out), optional :: origin\n    if (.not.associated(ptr)) error stop 91\n    ptr%value = val\n    if (present(stat)) stat = 0\n    if (present(origin)) origin = 11\n  end subroutine\nend module\n",
    );
    let array_src = write_program_in(
        &dir,
        "array_m.f90",
        "module array_m\n  use keyval_m, only : keyval_t, set_value\n  implicit none\n  type :: array_t\n    type(keyval_t), target :: node\n  end type\n  interface set_value\n    module procedure :: set_elem_value_float_sp\n    module procedure :: set_array_value_float_sp\n  end interface\ncontains\n  subroutine set_elem_value_float_sp(array, pos, val, stat, origin)\n    class(array_t), intent(inout) :: array\n    integer, intent(in) :: pos\n    real, intent(in) :: val\n    integer, intent(out), optional :: stat\n    integer, intent(out), optional :: origin\n    type(keyval_t), pointer :: ptr\n    if (pos /= 1) error stop 92\n    ptr => array%node\n    call set_value(ptr, val, stat, origin)\n  end subroutine\n  subroutine set_array_value_float_sp(array, val, stat, origin)\n    class(array_t), intent(inout) :: array\n    real, intent(in) :: val(:)\n    integer, intent(out), optional :: stat\n    integer, intent(out), optional :: origin\n    error stop 93\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use array_m, only : array_t, set_value\n  implicit none\n  type(array_t) :: array\n  integer :: stat\n  call set_value(array, 1, 3.5, stat=stat)\n  if (stat /= 0) error stop 94\n  if (abs(array%node%value - 3.5) > 1.0e-6) error stop 95\n  print *, 'ok'\nend program\n",
    );

    let keyval_obj = dir.join("keyval_m.o");
    let array_obj = dir.join("array_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("merged_generic_pointer_specific.bin");

    for (src, obj, needs_i) in [
        (&keyval_src, &keyval_obj, false),
        (&array_src, &array_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "merged generic pointer specific compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            keyval_obj.to_str().unwrap(),
            array_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "merged generic pointer specific link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run spawn failed");
    assert!(
        run.status.success(),
        "merged generic pointer specific run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected merged generic pointer specific output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merged_use_associated_generic_char_function_inside_concat_runs() {
    let dir = unique_dir("merged_generic_char_concat");
    let dt_src = write_program_in(
        &dir,
        "m_dt.f90",
        "module m_dt\n  implicit none\n  type :: dt_t\n    integer :: v = 0\n  end type\n  interface to_string\n    module procedure :: to_string_dt\n  end interface\ncontains\n  function to_string_dt(x) result(s)\n    type(dt_t), intent(in) :: x\n    character(len=:), allocatable :: s\n    s = \"dt\"\n  end function\nend module\n",
    );
    let num_src = write_program_in(
        &dir,
        "m_num.f90",
        "module m_num\n  implicit none\n  interface to_string\n    module procedure :: to_string_i4\n  end interface\ncontains\n  function to_string_i4(x) result(s)\n    integer, intent(in) :: x\n    character(len=:), allocatable :: s\n    s = \"int\"\n  end function\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program main\n  use m_dt, only : dt_t, to_string\n  use m_num\n  implicit none\n  type(dt_t) :: x\n  character(len=:), allocatable :: msg\n  x%v = 1\n  msg = \"expected '\" // to_string(x) // \"'\"\n  if (trim(msg) /= \"expected 'dt'\") error stop 91\n  print *, trim(msg)\nend program\n",
    );

    let dt_obj = dir.join("m_dt.o");
    let num_obj = dir.join("m_num.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("merged_generic_char_concat.bin");

    for (src, obj, needs_i) in [
        (&dt_src, &dt_obj, false),
        (&num_src, &num_obj, false),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "merged generic char concat compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            dt_obj.to_str().unwrap(),
            num_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("merged generic char concat link spawn failed");
    assert!(
        link.status.success(),
        "merged generic char concat should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("merged generic char concat run failed");
    assert!(
        run.status.success(),
        "merged generic char concat should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("expected 'dt'"),
        "unexpected merged generic char concat output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn module_subroutine_uses_datetime_stringifier_across_other_generic_modules() {
    let dir = unique_dir("module_subroutine_datetime_to_string");
    let dt_src = write_program_in(
        &dir,
        "m_dt.f90",
        "module m_dt\n  implicit none\n  type :: dt_t\n    integer :: v = 0\n  end type\n  interface to_string\n    module procedure :: to_string_dt\n  end interface\ncontains\n  function to_string_dt(x) result(s)\n    type(dt_t), intent(in) :: x\n    character(len=:), allocatable :: s\n    if (x%v == 1) then\n      s = \"dt\"\n    else\n      s = \"bad\"\n    end if\n  end function\nend module\n",
    );
    let td_src = write_program_in(
        &dir,
        "td_m.f90",
        "module td_m\n  implicit none\n  interface to_string\n    module procedure :: to_string_i4\n    module procedure :: to_string_r8\n  end interface\ncontains\n  function to_string_i4(x) result(s)\n    integer, intent(in) :: x\n    character(len=:), allocatable :: s\n    s = \"int\"\n  end function\n  function to_string_r8(x) result(s)\n    real(8), intent(in) :: x\n    character(len=:), allocatable :: s\n    s = \"real\"\n  end function\nend module\n",
    );
    let util_src = write_program_in(
        &dir,
        "util_m.f90",
        "module util_m\n  use m_dt, only : to_string\n  implicit none\n  private\n  public :: helper\n  public :: to_string\n  interface to_string\n    module procedure :: to_string_i2\n  end interface\ncontains\n  subroutine helper()\n  end subroutine\n  function to_string_i2(x) result(s)\n    integer(2), intent(in) :: x\n    character(len=:), allocatable :: s\n    s = \"i2\"\n  end function\nend module\n",
    );
    let lexer_src = write_program_in(
        &dir,
        "lexer_m.f90",
        "module lexer_m\n  use td_m\n  use m_dt, only : dt_t, to_string\n  use util_m, only : helper\n  implicit none\ncontains\n  subroutine run()\n    type(dt_t) :: ref\n    character(len=:), allocatable :: msg\n    call helper()\n    ref%v = 1\n    msg = \"expected '\" // to_string(ref) // \"'\"\n    if (trim(msg) /= \"expected 'dt'\") error stop 91\n    print *, trim(msg)\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program main\n  use lexer_m, only : run\n  implicit none\n  call run()\nend program\n",
    );

    let dt_obj = dir.join("m_dt.o");
    let td_obj = dir.join("td_m.o");
    let util_obj = dir.join("util_m.o");
    let lexer_obj = dir.join("lexer_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("module_subroutine_datetime_to_string.bin");

    for (src, obj, needs_i) in [
        (&dt_src, &dt_obj, false),
        (&td_src, &td_obj, false),
        (&util_src, &util_obj, true),
        (&lexer_src, &lexer_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "module-subroutine datetime to_string compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            dt_obj.to_str().unwrap(),
            td_obj.to_str().unwrap(),
            util_obj.to_str().unwrap(),
            lexer_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("module-subroutine datetime to_string link spawn failed");
    assert!(
        link.status.success(),
        "module-subroutine datetime to_string should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("module-subroutine datetime to_string run failed");
    assert!(
        run.status.success(),
        "module-subroutine datetime to_string run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("expected 'dt'"),
        "unexpected module-subroutine datetime to_string output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn local_name_semantic_type_beats_cross_scope_fallback_in_generic_dispatch() {
    let dir = unique_dir("generic_local_name_precedence");
    let noise_src = write_program_in(
        &dir,
        "noise_m.f90",
        "module noise_m\n  implicit none\ncontains\n  subroutine seed()\n    complex(8) :: val\n    val = (1.0d0, 2.0d0)\n    if (real(val) < 0.0d0) print *, val\n  end subroutine\nend module\n",
    );
    let dt_src = write_program_in(
        &dir,
        "m_dt.f90",
        "module m_dt\n  implicit none\n  type :: dt_t\n    integer :: v = 0\n  end type\n  interface to_string\n    module procedure :: to_string_dt\n  end interface\ncontains\n  function to_string_dt(x) result(s)\n    type(dt_t), intent(in) :: x\n    character(len=:), allocatable :: s\n    if (x%v == 1) then\n      s = \"dt\"\n    else\n      s = \"bad\"\n    end if\n  end function\nend module\n",
    );
    let driver_src = write_program_in(
        &dir,
        "driver_m.f90",
        "module driver_m\n  use noise_m, only : seed\n  use m_dt, only : dt_t, to_string\n  implicit none\ncontains\n  subroutine run()\n    type(dt_t) :: val\n    call seed()\n    val%v = 1\n    if (to_string(val) /= \"dt\") error stop 91\n    print *, 'ok'\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program main\n  use driver_m, only : run\n  implicit none\n  call run()\nend program\n",
    );

    let noise_obj = dir.join("noise_m.o");
    let dt_obj = dir.join("m_dt.o");
    let driver_obj = dir.join("driver_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("generic_local_name_precedence.bin");

    for (src, obj, needs_i) in [
        (&noise_src, &noise_obj, false),
        (&dt_src, &dt_obj, false),
        (&driver_src, &driver_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "generic local-name precedence compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            noise_obj.to_str().unwrap(),
            dt_obj.to_str().unwrap(),
            driver_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("generic local-name precedence link spawn failed");
    assert!(
        link.status.success(),
        "generic local-name precedence should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("generic local-name precedence run failed");
    assert!(
        run.status.success(),
        "generic local-name precedence run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected generic local-name precedence output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn local_pointer_semantic_type_beats_cross_scope_class_fallback_in_generic_dispatch() {
    let dir = unique_dir("generic_local_pointer_precedence");
    let noise_src = write_program_in(
        &dir,
        "noise_m.f90",
        "module noise_m\n  implicit none\n  type, abstract :: base_t\n  end type\ncontains\n  subroutine seed()\n    class(base_t), pointer :: ptr\n    nullify(ptr)\n  end subroutine\nend module\n",
    );
    let tables_src = write_program_in(
        &dir,
        "tables_m.f90",
        "module tables_m\n  implicit none\n  type :: table_t\n    integer :: mark = 0\n  end type\n  interface add_table\n    module procedure :: add_table_to_table\n  end interface\ncontains\n  subroutine add_table_to_table(table, key, ptr, stat)\n    class(table_t), intent(inout) :: table\n    character(len=*), intent(in) :: key\n    type(table_t), pointer, intent(out) :: ptr\n    integer, intent(out), optional :: stat\n    nullify(ptr)\n    table%mark = len_trim(key)\n    if (present(stat)) stat = 0\n  end subroutine\n  subroutine add_table_key(table, key, ptr, stat)\n    class(table_t), intent(inout) :: table\n    character(len=*), intent(in) :: key\n    type(table_t), pointer, intent(out) :: ptr\n    integer, intent(out), optional :: stat\n    call add_table(table, key, ptr, stat)\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program main\n  use noise_m, only : seed\n  use tables_m, only : table_t, add_table_key\n  implicit none\n  type(table_t) :: table\n  type(table_t), pointer :: ptr\n  integer :: stat\n  call seed()\n  call add_table_key(table, 'abc', ptr, stat)\n  if (stat /= 0) error stop 91\n  if (table%mark /= 3) error stop 92\n  print *, 'ok'\nend program\n",
    );

    let noise_obj = dir.join("noise_m.o");
    let tables_obj = dir.join("tables_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("generic_local_pointer_precedence.bin");

    for (src, obj, needs_i) in [
        (&noise_src, &noise_obj, false),
        (&tables_src, &tables_obj, false),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "generic local-pointer precedence compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            noise_obj.to_str().unwrap(),
            tables_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("generic local-pointer precedence link spawn failed");
    assert!(
        link.status.success(),
        "generic local-pointer precedence should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("generic local-pointer precedence run failed");
    assert!(
        run.status.success(),
        "generic local-pointer precedence run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected generic local-pointer precedence output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn merged_generic_keeps_same_named_specifics_scoped_to_owner_module() {
    let dir = unique_dir("merged_generic_owner_scope");
    let storage_src = write_program_in(
        &dir,
        "storage_m.f90",
        "module storage_m\n  implicit none\n  type :: storage_t\n  contains\n    procedure :: get_len\n  end type\ncontains\n  pure integer function get_len(self) result(length)\n    class(storage_t), intent(in) :: self\n    length = 111\n  end function\nend module\n",
    );
    let array_src = write_program_in(
        &dir,
        "array_m.f90",
        "module array_m\n  use storage_m, only : storage_t\n  implicit none\n  type :: box_t\n    type(storage_t) :: storage\n  end type\n  interface len\n    module procedure :: get_len\n  end interface\ncontains\n  pure integer function get_len(self) result(length)\n    class(box_t), intent(in) :: self\n    length = 222\n  end function\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use array_m, only : box_t, len\n  implicit none\n  type(box_t) :: box\n  if (len(box) /= 222) error stop 1\n  print *, 'ok'\nend program\n",
    );

    let storage_obj = dir.join("storage_m.o");
    let array_obj = dir.join("array_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("merged_generic_owner_scope.bin");

    for (src, obj, needs_i) in [
        (&storage_src, &storage_obj, false),
        (&array_src, &array_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "merged generic owner-scope compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            storage_obj.to_str().unwrap(),
            array_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "merged generic owner-scope link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run spawn failed");
    assert!(
        run.status.success(),
        "merged generic owner-scope run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected merged generic owner-scope output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn intrinsic_len_falls_back_when_visible_generic_len_does_not_match() {
    let src = write_program(
        "module m\n  implicit none\n  type :: box_t\n  end type\n  interface len\n    module procedure :: get_len\n  end interface\ncontains\n  integer function get_len(self) result(length)\n    class(box_t), intent(in) :: self\n    length = 7\n  end function\n  integer function escaped_len(raw) result(length)\n    character(len=*), intent(in) :: raw\n    length = len(raw)\n  end function\nend module\nprogram p\n  use m, only : box_t, len, escaped_len\n  implicit none\n  type(box_t) :: box\n  if (len(box) /= 7) error stop 1\n  if (escaped_len('abc') /= 3) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("intrinsic_len_generic_fallback", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("intrinsic len generic fallback compile failed to spawn");
    assert!(
        compile.status.success(),
        "intrinsic len generic fallback compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("intrinsic len generic fallback run failed");
    assert!(
        run.status.success(),
        "intrinsic len generic fallback run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected intrinsic len generic fallback output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn generic_subroutine_accepts_derived_array_element_actual() {
    let src = write_program(
        "module m\n  implicit none\n  type :: dt\n    integer :: n = 0\n  end type\n  interface get_value\n    module procedure :: get_dt\n  end interface\ncontains\n  subroutine get_dt(array, pos, val, stat)\n    integer, intent(in) :: array\n    integer, intent(in) :: pos\n    type(dt), intent(out) :: val\n    integer, intent(out), optional :: stat\n    val%n = array + pos\n    if (present(stat)) stat = 0\n  end subroutine\n  subroutine fill(vals)\n    type(dt), allocatable, intent(out) :: vals(:)\n    integer :: i, info\n    allocate(vals(2))\n    do i = 1, size(vals)\n      call get_value(10, i, vals(i), info)\n      if (info /= 0) error stop 1\n    end do\n  end subroutine\nend module\nprogram p\n  use m, only : dt, fill\n  implicit none\n  type(dt), allocatable :: vals(:)\n  call fill(vals)\n  if (vals(1)%n /= 11 .or. vals(2)%n /= 12) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("generic_derived_array_elem", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("generic derived array elem compile failed to spawn");
    assert!(
        compile.status.success(),
        "generic derived array elem compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("generic derived array elem run failed");
    assert!(
        run.status.success(),
        "generic derived array elem run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected generic derived array elem output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn generic_subroutine_accepts_imported_derived_array_element_actual() {
    let dir = unique_dir("generic_imported_derived_array_elem");
    let types_src = write_program_in(
        &dir,
        "types.f90",
        "module types_m\n  implicit none\n  type :: dt\n    integer :: n = 0\n  end type\nend module\n",
    );
    let impl_src = write_program_in(
        &dir,
        "impl.f90",
        "module impl_m\n  use types_m, only : dt\n  implicit none\n  interface get_value\n    module procedure :: get_dt\n  end interface\ncontains\n  subroutine get_dt(array, pos, val, stat, origin)\n    integer, intent(in) :: array\n    integer, intent(in) :: pos\n    type(dt), intent(out) :: val\n    integer, intent(out), optional :: stat\n    integer, intent(out), optional :: origin\n    val%n = array + pos\n    if (present(stat)) stat = 0\n    if (present(origin)) origin = pos\n  end subroutine\n  subroutine fill(vals)\n    type(dt), allocatable, intent(out) :: vals(:)\n    integer :: i, info, origin\n    allocate(vals(2))\n    do i = 1, size(vals)\n      call get_value(10, i, vals(i), info, origin)\n      if (info /= 0) error stop 1\n    end do\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use types_m, only : dt\n  use impl_m, only : fill\n  implicit none\n  type(dt), allocatable :: vals(:)\n  call fill(vals)\n  if (vals(1)%n /= 11 .or. vals(2)%n /= 12) error stop 2\n  print *, 'ok'\nend program\n",
    );
    let types_obj = dir.join("types.o");
    let impl_obj = dir.join("impl.o");
    let main_obj = dir.join("main.o");
    let out = dir.join("generic_imported_derived_array_elem");

    for (src, obj) in [(&types_src, &types_obj), (&impl_src, &impl_obj), (&main_src, &main_obj)] {
        let compile = Command::new(compiler("armfortas"))
            .current_dir(&dir)
            .args([
                src.to_str().unwrap(),
                "-c",
                "-J",
                dir.to_str().unwrap(),
                "-o",
                obj.to_str().unwrap(),
            ])
            .output()
            .expect("imported derived array elem compile failed to spawn");
        assert!(
            compile.status.success(),
            "imported derived array elem compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            types_obj.to_str().unwrap(),
            impl_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("imported derived array elem link failed to spawn");
    assert!(
        link.status.success(),
        "imported derived array elem link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("imported derived array elem run failed");
    assert!(
        run.status.success(),
        "imported derived array elem run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected imported derived array elem output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scalar_pointer_actual_materializes_descriptor_for_class_dummy() {
    let src = write_program(
        "module m\n  implicit none\n  type :: box_t\n    integer :: n = 0\n  contains\n    procedure :: set_n\n  end type\ncontains\n  subroutine set_n(self, v)\n    class(box_t), intent(inout) :: self\n    integer, intent(in) :: v\n    self%n = v\n  end subroutine\n  subroutine set_box(self, v)\n    class(box_t), intent(inout) :: self\n    integer, intent(in) :: v\n    call self%set_n(v)\n  end subroutine\nend module\nprogram p\n  use m, only : box_t, set_box\n  implicit none\n  type(box_t), pointer :: ptr\n  allocate(ptr)\n  call set_box(ptr, 7)\n  if (.not.associated(ptr)) error stop 1\n  if (ptr%n /= 7) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("pointer_class_dummy_descriptor", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("pointer class dummy descriptor compile failed to spawn");
    assert!(
        compile.status.success(),
        "pointer class dummy descriptor compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("pointer class dummy descriptor run failed");
    assert!(
        run.status.success(),
        "pointer class dummy descriptor run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected pointer class dummy descriptor output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_allocated_concrete_moved_into_class_component_dispatches_concrete_len() {
    let dir = unique_dir("imported_concrete_move_alloc_dispatch");
    let base_src = write_program_in(
        &dir,
        "base_m.f90",
        "module base_m\n  implicit none\n  type, abstract :: list_t\n  contains\n    procedure(get_len_i), deferred :: get_len\n  end type\n  abstract interface\n    integer function get_len_i(self) result(length)\n      import :: list_t\n      class(list_t), intent(in) :: self\n    end function\n  end interface\nend module\n",
    );
    let impl_src = write_program_in(
        &dir,
        "impl_m.f90",
        "module impl_m\n  use base_m, only : list_t\n  implicit none\n  type, extends(list_t) :: list_impl_t\n    integer :: n = 0\n  contains\n    procedure :: get_len => impl_get_len\n  end type\ncontains\n  subroutine new_list_impl(self)\n    type(list_impl_t), intent(out) :: self\n    self%n = 17\n  end subroutine\n  integer function impl_get_len(self) result(length)\n    class(list_impl_t), intent(in) :: self\n    length = self%n\n  end function\nend module\n",
    );
    let factory_src = write_program_in(
        &dir,
        "factory_m.f90",
        "module factory_m\n  use base_m, only : list_t\n  use impl_m, only : list_impl_t, new_list_impl\n  implicit none\ncontains\n  subroutine make_list(self)\n    class(list_t), allocatable, intent(out) :: self\n    block\n      type(list_impl_t), allocatable :: list\n      allocate(list)\n      call new_list_impl(list)\n      call move_alloc(list, self)\n    end block\n  end subroutine\nend module\n",
    );
    let array_src = write_program_in(
        &dir,
        "array_m.f90",
        "module array_m\n  use base_m, only : list_t\n  use factory_m, only : make_list\n  implicit none\n  type :: array_t\n    class(list_t), allocatable :: list\n  contains\n    procedure :: init\n    procedure :: get_len => array_get_len\n  end type\ncontains\n  subroutine init(self)\n    class(array_t), intent(out) :: self\n    call make_list(self%list)\n  end subroutine\n  integer function array_get_len(self) result(length)\n    class(array_t), intent(in) :: self\n    length = self%list%get_len()\n  end function\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use array_m, only : array_t\n  implicit none\n  type(array_t) :: arr\n  call arr%init()\n  if (arr%get_len() /= 17) error stop 1\n  print *, 'ok'\nend program\n",
    );

    let base_obj = dir.join("base_m.o");
    let impl_obj = dir.join("impl_m.o");
    let factory_obj = dir.join("factory_m.o");
    let array_obj = dir.join("array_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("imported_concrete_move_alloc_dispatch.bin");

    for (src, obj, needs_i) in [
        (&base_src, &base_obj, false),
        (&impl_src, &impl_obj, true),
        (&factory_src, &factory_obj, true),
        (&array_src, &array_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "cross-tu move_alloc dispatch compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            base_obj.to_str().unwrap(),
            impl_obj.to_str().unwrap(),
            factory_obj.to_str().unwrap(),
            array_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("cross-tu move_alloc dispatch link spawn failed");
    assert!(
        link.status.success(),
        "cross-tu move_alloc dispatch should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("cross-tu move_alloc dispatch run failed");
    assert!(
        run.status.success(),
        "cross-tu move_alloc dispatch runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected cross-tu move_alloc dispatch output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_generic_keyword_real_expression_actual_resolves() {
    let dir = unique_dir("generic_imported_keyword_real_expr");
    let mod_src = write_program_in(
        &dir,
        "testdrive.f90",
        "module testdrive\n  implicit none\n  type :: error_type\n    integer :: dummy = 0\n  end type\n  interface check\n    module procedure :: check_float_dp\n  end interface\ncontains\n  subroutine check_float_dp(error, actual, expected, message, more, thr, rel)\n    type(error_type), allocatable, intent(out) :: error\n    real(8), intent(in) :: actual\n    real(8), intent(in) :: expected\n    character(len=*), intent(in), optional :: message\n    character(len=*), intent(in), optional :: more\n    real(8), intent(in), optional :: thr\n    logical, intent(in), optional :: rel\n    real(8) :: tol\n    tol = 0.0d0\n    if (present(thr)) tol = thr\n    if (abs(actual - expected) > tol) allocate(error)\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use testdrive\n  implicit none\n  type(error_type), allocatable :: error\n  real(8) :: val1, val2\n  val1 = 1.0d0\n  val2 = 1.0d0 + epsilon(val1)\n  call check(error, val2, val1, thr=2*epsilon(val1))\n  if (allocated(error)) error stop 1\n  print *, 'ok'\nend program\n",
    );

    let mod_obj = dir.join("testdrive.o");
    let main_obj = dir.join("main.o");
    let out = dir.join("generic_imported_keyword_real_expr.bin");

    for (src, obj) in [(&mod_src, &mod_obj), (&main_src, &main_obj)] {
        let compile = Command::new(compiler("armfortas"))
            .current_dir(&dir)
            .args([
                "-c",
                "-I",
                dir.to_str().unwrap(),
                "-J",
                dir.to_str().unwrap(),
                src.to_str().unwrap(),
                "-o",
                obj.to_str().unwrap(),
            ])
            .output()
            .expect("imported generic keyword real expr compile failed to spawn");
        assert!(
            compile.status.success(),
            "imported generic keyword real expr compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            mod_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("imported generic keyword real expr link failed to spawn");
    assert!(
        link.status.success(),
        "imported generic keyword real expr link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("imported generic keyword real expr run failed");
    assert!(
        run.status.success(),
        "imported generic keyword real expr run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected imported generic keyword real expr output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn generic_function_resolution_uses_specific_keyword_slots() {
    let src = write_program(
        "module m\n  implicit none\n  type :: stamp_t\n    integer :: year = -1\n    integer :: month = -1\n    integer :: day = -1\n    integer :: hour = -1\n    integer :: minute = -1\n    integer :: second = -1\n  end type\n  interface build_stamp\n    module procedure :: build_stamp_fields\n    module procedure :: build_stamp_string\n  end interface\ncontains\n  function build_stamp_fields(year, month, day, hour, minute, second) result(stamp)\n    integer, intent(in), optional :: year, month, day\n    integer, intent(in), optional :: hour, minute, second\n    type(stamp_t) :: stamp\n    if (present(year)) stamp%year = year\n    if (present(month)) stamp%month = month\n    if (present(day)) stamp%day = day\n    if (present(hour)) stamp%hour = hour\n    if (present(minute)) stamp%minute = minute\n    if (present(second)) stamp%second = second\n  end function\n\n  function build_stamp_string(string) result(stamp)\n    character(len=*), intent(in) :: string\n    type(stamp_t) :: stamp\n    stamp%year = len_trim(string)\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  type(stamp_t) :: stamp\n  stamp = build_stamp(hour=17, minute=45, second=0)\n  if (stamp%year /= -1) error stop 1\n  if (stamp%month /= -1) error stop 2\n  if (stamp%day /= -1) error stop 3\n  if (stamp%hour /= 17) error stop 4\n  if (stamp%minute /= 45) error stop 5\n  if (stamp%second /= 0) error stop 6\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("generic_function_keyword_slots", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("generic function keyword slots compile failed to spawn");
    assert!(
        compile.status.success(),
        "generic function keyword slots compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("generic function keyword slots run failed");
    assert!(
        run.status.success(),
        "generic function keyword slots run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected generic function keyword slots output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn random_number_default_real_in_hidden_result_function_uses_scalar_width() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=15) :: name\n  name = get_name()\n  if (name(1:7) /= 'toml-f-') error stop 1\n  print *, trim(name)\ncontains\n  function get_name() result(filename)\n    character(len=15) :: filename\n    real :: val\n    call random_number(val)\n    write(filename, '(a, z8.8)') 'toml-f-', int(val*1.0e9)\n  end function\nend program\n",
        "f90",
    );
    let out = unique_path("random_number_hidden_result_f32", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("random_number hidden-result f32 compile failed to spawn");
    assert!(
        compile.status.success(),
        "random_number hidden-result f32 compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("random_number hidden-result f32 run failed");
    assert!(
        run.status.success(),
        "random_number hidden-result f32 run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("toml-f-"),
        "unexpected random_number hidden-result f32 output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn open_connected_unit_without_file_preserves_existing_connection() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: io, stat, chunk\n  logical :: opened\n  character(4096) :: buffer\n  character(len=*), parameter :: filename = 'open_connected_unit_repro.txt'\n\n  open(file=filename, newunit=io, status='replace')\n  write(io, '(a)') 'abc'\n  close(io)\n\n  open(file=filename, newunit=io, status='old')\n  inquire(unit=io, opened=opened)\n  if (.not. opened) error stop 1\n  open(unit=io, pad='yes', iostat=stat)\n  if (stat /= 0) error stop 2\n  read(io, '(a)', advance='no', iostat=stat, size=chunk) buffer\n  if (stat > 0) error stop 3\n  if (chunk /= 3) error stop 4\n  if (buffer(:chunk) /= 'abc') error stop 5\n  close(io, status='delete')\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("open_connected_unit_reuses_file", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("open connected unit compile failed to spawn");
    assert!(
        compile.status.success(),
        "open connected unit compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("open connected unit run failed");
    assert!(
        run.status.success(),
        "open connected unit run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected open connected unit output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn open_connected_unit_without_file_does_not_rewind_nonadvancing_reads() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: io, stat\n  character(:), allocatable :: line\n  character(len=*), parameter :: filename = 'open_connected_unit_loop_repro.txt'\n\n  open(file=filename, newunit=io, status='replace')\n  write(io, '(a)') 'abc'\n  close(io)\n\n  open(file=filename, newunit=io, status='old')\n  call read_line(io, line, stat)\n  if (stat /= 0) error stop 1\n  if (line /= 'abc') error stop 2\n  call read_line(io, line, stat)\n  if (stat >= 0) error stop 3\n  close(io, status='delete')\n  print *, 'ok'\ncontains\n  subroutine read_line(io, string, stat)\n    integer, intent(in) :: io\n    character(:), allocatable, intent(out) :: string\n    integer, intent(out) :: stat\n    integer, parameter :: bufsize = 16\n    character(bufsize) :: buffer\n    integer :: chunk\n\n    open(unit=io, pad='yes', iostat=stat)\n    string = ''\n    do while (stat == 0)\n      read(io, '(a)', advance='no', iostat=stat, size=chunk) buffer\n      if (stat > 0) exit\n      string = string // buffer(:chunk)\n    end do\n    if (is_iostat_eor(stat)) stat = 0\n  end subroutine\nend program\n",
        "f90",
    );
    let out = unique_path("open_connected_unit_no_rewind", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("open connected unit no-rewind compile failed to spawn");
    assert!(
        compile.status.success(),
        "open connected unit no-rewind compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("open connected unit no-rewind run failed");
    assert!(
        run.status.success(),
        "open connected unit no-rewind run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected open connected unit no-rewind output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_generic_unary_array_element_actual_preserves_integer_kind() {
    let dir = unique_dir("generic_imported_unary_array_element_kind");
    let mod_src = write_program_in(
        &dir,
        "testdrive.f90",
        "module testdrive\n  implicit none\n  type :: error_type\n    integer :: dummy = 0\n  end type\n  interface check\n    module procedure :: check_int_i2\n  end interface\ncontains\n  subroutine check_int_i2(error, actual, expected, message, more)\n    type(error_type), allocatable, intent(out) :: error\n    integer(2), intent(in) :: actual\n    integer(2), intent(in) :: expected\n    character(len=*), intent(in), optional :: message\n    character(len=*), intent(in), optional :: more\n    if (actual /= expected) allocate(error)\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use testdrive\n  implicit none\n  type(error_type), allocatable :: error\n  integer(2), parameter :: ref(4) = [integer(2) :: 1, 3, 5, 7]\n  integer(2) :: val\n  integer :: ii\n  ii = 2\n  val = -ref(ii)\n  call check(error, val, -ref(ii))\n  if (allocated(error)) error stop 1\n  print *, 'ok'\nend program\n",
    );

    let mod_obj = dir.join("testdrive.o");
    let main_obj = dir.join("main.o");
    let out = dir.join("generic_imported_unary_array_element_kind.bin");

    for (src, obj) in [(&mod_src, &mod_obj), (&main_src, &main_obj)] {
        let compile = Command::new(compiler("armfortas"))
            .current_dir(&dir)
            .args([
                "-c",
                "-I",
                dir.to_str().unwrap(),
                "-J",
                dir.to_str().unwrap(),
                src.to_str().unwrap(),
                "-o",
                obj.to_str().unwrap(),
            ])
            .output()
            .expect("imported generic unary array element compile failed to spawn");
        assert!(
            compile.status.success(),
            "imported generic unary array element compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            mod_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("imported generic unary array element link failed to spawn");
    assert!(
        link.status.success(),
        "imported generic unary array element link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("imported generic unary array element run failed");
    assert!(
        run.status.success(),
        "imported generic unary array element run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected imported generic unary array element output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn assumed_size_parameter_array_constructor_uses_initializer_extent() {
    let src = write_program(
        "module m\n  implicit none\ncontains\n  elemental function peek(chunk, pos) result(ch)\n    character(*), intent(in) :: chunk\n    integer, intent(in) :: pos\n    character(1) :: ch\n    if (pos <= len(chunk)) then\n      ch = chunk(pos:pos)\n    else\n      ch = ' '\n    end if\n  end function\n  pure function match_all(chunk, pos, kind) result(match)\n    character(*), intent(in) :: chunk\n    integer, intent(in) :: pos(:)\n    character(1), intent(in) :: kind(:)\n    logical :: match\n    match = all(peek(chunk, pos) == kind)\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  integer, parameter :: offset(*) = [1, 2, 3, 4]\n  character(1), parameter :: truth(4) = ['t', 'r', 'u', 'e']\n  logical :: ok\n  ok = match_all('true', offset, truth)\n  if (.not. ok) error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("assumed_size_parameter_initializer_extent", "bin");

    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("assumed-size parameter initializer compile failed to spawn");
    assert!(
        compile.status.success(),
        "assumed-size parameter initializer compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("assumed-size parameter initializer run failed");
    assert!(
        run.status.success(),
        "assumed-size parameter initializer run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected assumed-size parameter initializer output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn module_character_parameter_array_constructor_initializes_runtime_bytes() {
    let src = write_program(
        "module m\n  implicit none\n  integer, parameter :: offset(4) = [0, 1, 2, 3]\n  character(1), parameter :: truth(4) = ['t', 'r', 'u', 'e']\ncontains\n  elemental function peek(chunk, pos) result(ch)\n    character(*), intent(in) :: chunk\n    integer, intent(in) :: pos\n    character(1) :: ch\n    if (pos <= len(chunk)) then\n      ch = chunk(pos:pos)\n    else\n      ch = ' '\n    end if\n  end function\n  pure function match_all(chunk, pos, kind) result(match)\n    character(*), intent(in) :: chunk\n    integer, intent(in) :: pos(:)\n    character(1), intent(in) :: kind(:)\n    logical :: match\n    match = all(peek(chunk, pos) == kind)\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  logical :: ok\n  if (iachar(truth(1)) /= 116) error stop 1\n  if (iachar(truth(2)) /= 114) error stop 2\n  if (iachar(truth(3)) /= 117) error stop 3\n  if (iachar(truth(4)) /= 101) error stop 4\n  ok = match_all('true', 1 + offset, truth)\n  if (.not. ok) error stop 5\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("module_char_parameter_array_init", "bin");

    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("module char parameter array init compile failed to spawn");
    assert!(
        compile.status.success(),
        "module char parameter array init compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("module char parameter array init run failed");
    assert!(
        run.status.success(),
        "module char parameter array init run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected module char parameter array init output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn elemental_character_compare_uses_hidden_result_bytes() {
    let src = write_program(
        "module m\n  implicit none\ncontains\n  elemental function peek(chunk, pos) result(ch)\n    character(*), intent(in) :: chunk\n    integer, intent(in) :: pos\n    character(1) :: ch\n    if (pos <= len(chunk)) then\n      ch = chunk(pos:pos)\n    else\n      ch = ' '\n    end if\n  end function\n  pure function match_all(chunk, pos, kind) result(match)\n    character(*), intent(in) :: chunk\n    integer, intent(in) :: pos(:)\n    character(1), intent(in) :: kind(:)\n    logical :: match\n    match = all(peek(chunk, pos) == kind)\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  integer, parameter :: offset(*) = [1, 2, 3]\n  character(1), parameter :: truth(3) = ['t', 'r', 'u']\n  logical :: ok\n  ok = match_all('tru', offset, truth)\n  if (.not. ok) error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("elemental_char_compare_hidden_result", "bin");

    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("elemental char compare hidden-result compile failed to spawn");
    assert!(
        compile.status.success(),
        "elemental char compare hidden-result compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("elemental char compare hidden-result run failed");
    assert!(
        run.status.success(),
        "elemental char compare hidden-result run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected elemental char compare hidden-result output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn opposite_quote_runs_preserve_fixed_character_lengths() {
    let src = write_program(
        r#"program p
  implicit none
  if (len("'''") /= 3) error stop 1
  if ("'''" /= "'''") error stop 2
  if (len('"""') /= 3) error stop 3
  if ('"""' /= '"""') error stop 4
  print *, 'ok'
end program
"#,
        "f90",
    );
    let out = unique_path("opposite_quote_lengths", "bin");

    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("opposite quote length compile failed to spawn");
    assert!(
        compile.status.success(),
        "opposite quote length compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("opposite quote length run failed");
    assert!(
        run.status.success(),
        "opposite quote length run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected opposite quote length output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn elemental_character_call_over_array_expr_materializes_descriptor_actual() {
    let src = write_program(
        "module m\n  implicit none\ncontains\n  elemental function pick(pos) result(ch)\n    integer, intent(in) :: pos\n    character(1) :: ch\n    ch = merge('X', 'Y', pos > 0)\n  end function\n\n  logical function want_ten_chars(string)\n    character(1), intent(in) :: string(:)\n    want_ten_chars = string(5) == 'X'\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  integer, parameter :: offset(*) = [0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10]\n  integer, parameter :: offset_date = 10\n  if (.not. want_ten_chars(pick(1 + offset(:offset_date)))) error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("elemental_char_array_descriptor_actual", "bin");

    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("elemental char array descriptor actual compile failed to spawn");
    assert!(
        compile.status.success(),
        "elemental char array descriptor actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("elemental char array descriptor actual run failed");
    assert!(
        run.status.success(),
        "elemental char array descriptor actual run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected elemental char array descriptor actual output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn module_character_star_parameter_from_component_concat_initializes_runtime_bytes() {
    let src = write_program(
        "module m\n  implicit none\n  type :: enum_char\n    character(1) :: space = ' '\n    character(1) :: hash = '#'\n    character(1) :: comma = ','\n  end type\n  type(enum_char), parameter :: char_kind = enum_char()\n  character(len=*), parameter :: terminated = char_kind%space // char_kind%hash // char_kind%comma\ncontains\n  subroutine check()\n    if (len(terminated) /= 3) error stop 1\n    if (iachar(terminated(1:1)) /= 32) error stop 2\n    if (iachar(terminated(2:2)) /= 35) error stop 3\n    if (iachar(terminated(3:3)) /= 44) error stop 4\n    print *, 'ok'\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  call check()\nend program\n",
        "f90",
    );
    let out = unique_path("module_char_star_component_concat", "bin");

    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("module char star component concat compile failed to spawn");
    assert!(
        compile.status.success(),
        "module char star component concat compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("module char star component concat run failed");
    assert!(
        run.status.success(),
        "module char star component concat run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected module char star component concat output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn module_character_star_parameter_with_achar_and_repeat_defaults_initializes_runtime_bytes() {
    let src = write_program(
        "module m\n  implicit none\n  integer, parameter :: tfc = kind('a')\n  type :: enum_char\n    character(1, tfc) :: space = \" \"\n    character(1, tfc) :: hash = \"#\"\n    character(3, tfc) :: squote3 = repeat(\"'\", 3)\n    character(1, tfc) :: comma = \",\"\n    character(1, tfc) :: equal = \"=\"\n    character(1, tfc) :: rbrace = \"}\"\n    character(1, tfc) :: rbracket = \"]\"\n    character(1, tfc) :: newline = achar(10, kind=tfc)\n    character(1, tfc) :: carriage_return = achar(13, kind=tfc)\n    character(1, tfc) :: tab = achar(9, kind=tfc)\n  end type\n  type(enum_char), parameter :: char_kind = enum_char()\n  character(*, tfc), parameter :: terminated = &\n    char_kind%space // char_kind%tab // char_kind%newline // char_kind%carriage_return // &\n    char_kind%hash // char_kind%rbrace // char_kind%rbracket // char_kind%comma // &\n    char_kind%equal\ncontains\n  subroutine check()\n    if (len(terminated) /= 9) error stop 1\n    if (iachar(terminated(1:1)) /= 32) error stop 2\n    if (iachar(terminated(2:2)) /= 9) error stop 3\n    if (iachar(terminated(3:3)) /= 10) error stop 4\n    if (iachar(terminated(4:4)) /= 13) error stop 5\n    if (terminated(5:9) /= '#}],=') error stop 6\n    print *, 'ok'\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  call check()\nend program\n",
        "f90",
    );
    let out = unique_path("module_char_star_achar_repeat", "bin");

    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("module char star achar/repeat compile failed to spawn");
    assert!(
        compile.status.success(),
        "module char star achar/repeat compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("module char star achar/repeat run failed");
    assert!(
        run.status.success(),
        "module char star achar/repeat run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected module char star achar/repeat output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn polymorphic_name_bound_calls_dispatch_for_visitor_and_destroy() {
    let src = write_program(
        "module m\n  implicit none\n  type, abstract :: node_t\n  contains\n    procedure(destroy_i), deferred :: destroy\n  end type\n  type, abstract :: visitor_t\n  contains\n    procedure(visit_i), deferred :: visit\n  end type\n  abstract interface\n    subroutine destroy_i(self)\n      import :: node_t\n      class(node_t), intent(inout) :: self\n    end subroutine\n    subroutine visit_i(self, val)\n      import :: visitor_t, node_t\n      class(visitor_t), intent(inout) :: self\n      class(node_t), intent(inout) :: val\n    end subroutine\n  end interface\n  type, extends(node_t) :: leaf_t\n  contains\n    procedure :: destroy => leaf_destroy\n  end type\n  type, extends(visitor_t) :: print_visitor_t\n  contains\n    procedure :: visit => print_visit\n  end type\ncontains\n  subroutine leaf_destroy(self)\n    class(leaf_t), intent(inout) :: self\n    print *, 'destroy'\n  end subroutine\n  subroutine print_visit(self, val)\n    class(print_visitor_t), intent(inout) :: self\n    class(node_t), intent(inout) :: val\n    print *, 'visit'\n    call val%destroy()\n  end subroutine\n  subroutine run()\n    class(node_t), allocatable :: val\n    class(visitor_t), allocatable :: vis\n    allocate(leaf_t :: val)\n    allocate(print_visitor_t :: vis)\n    call vis%visit(val)\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  call run()\nend program\n",
        "f90",
    );
    let out = unique_path("polymorphic_name_bound_dispatch", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("polymorphic name bound dispatch compile failed to spawn");
    assert!(
        compile.status.success(),
        "polymorphic name bound dispatch should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("polymorphic name bound dispatch run failed");
    assert!(
        run.status.success(),
        "polymorphic name bound dispatch should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("visit") && stdout.contains("destroy"),
        "unexpected polymorphic name bound dispatch output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn cross_tu_polymorphic_component_bound_call_uses_descriptor_lookup() {
    let dir = unique_dir("cross_tu_polymorphic_component_bound_dispatch");
    let base_src = write_program_in(
        &dir,
        "base_m.f90",
        "module base_m\n  implicit none\n  type, abstract :: base_t\n  contains\n    procedure(destroy_i), deferred :: destroy\n  end type\n  abstract interface\n    subroutine destroy_i(self)\n      import :: base_t\n      class(base_t), intent(inout) :: self\n    end subroutine\n  end interface\n  type :: node_t\n    class(base_t), allocatable :: val\n  end type\n  type :: holder_t\n    type(node_t), allocatable :: items(:)\n  contains\n    procedure :: zap\n  end type\ncontains\n  subroutine zap(self)\n    class(holder_t), intent(inout) :: self\n    call self%items(1)%val%destroy()\n  end subroutine\nend module\n",
    );
    let impl_src = write_program_in(
        &dir,
        "impl_m.f90",
        "module impl_m\n  use base_m, only : base_t\n  implicit none\n  type, extends(base_t) :: leaf_t\n  contains\n    procedure :: destroy => leaf_destroy\n  end type\ncontains\n  subroutine leaf_destroy(self)\n    class(leaf_t), intent(inout) :: self\n    print *, 'destroy'\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use base_m, only : holder_t\n  use impl_m, only : leaf_t\n  implicit none\n  type(holder_t) :: holder\n  allocate(holder%items(1))\n  allocate(leaf_t :: holder%items(1)%val)\n  call holder%zap()\n  print *, 'ok'\nend program\n",
    );

    let base_obj = dir.join("base_m.o");
    let impl_obj = dir.join("impl_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("cross_tu_polymorphic_component_bound_dispatch.bin");

    for (src, obj, needs_i) in [
        (&base_src, &base_obj, false),
        (&impl_src, &impl_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "cross-tu polymorphic component dispatch compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            base_obj.to_str().unwrap(),
            impl_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("cross-tu polymorphic component dispatch link spawn failed");
    assert!(
        link.status.success(),
        "cross-tu polymorphic component dispatch should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("cross-tu polymorphic component dispatch run failed");
    assert!(
        run.status.success(),
        "cross-tu polymorphic component dispatch should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("destroy") && stdout.contains("ok"),
        "unexpected cross-tu polymorphic component dispatch output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_polymorphic_name_bound_call_uses_descriptor_lookup() {
    let dir = unique_dir("cross_tu_polymorphic_name_bound_dispatch");
    let base_src = write_program_in(
        &dir,
        "base_m.f90",
        "module base_m\n  implicit none\n  type, abstract :: value_t\n  contains\n    procedure :: accept\n  end type\n  type, extends(value_t) :: leaf_t\n    integer :: payload = 17\n  end type\n  type, abstract :: visitor_t\n  contains\n    procedure(visit_i), deferred :: visit\n  end type\n  abstract interface\n    subroutine visit_i(self, val)\n      import :: visitor_t, value_t\n      class(visitor_t), intent(inout) :: self\n      class(value_t), intent(inout) :: val\n    end subroutine\n  end interface\ncontains\n  subroutine accept(self, visitor)\n    class(value_t), intent(inout) :: self\n    class(visitor_t), intent(inout) :: visitor\n    call visitor%visit(self)\n  end subroutine\nend module\n",
    );
    let impl_src = write_program_in(
        &dir,
        "impl_m.f90",
        "module impl_m\n  use base_m, only : visitor_t, value_t, leaf_t\n  implicit none\n  type, extends(visitor_t) :: counting_visitor_t\n    integer :: seen = -1\n  contains\n    procedure :: visit => visit_leaf\n  end type\ncontains\n  subroutine visit_leaf(self, val)\n    class(counting_visitor_t), intent(inout) :: self\n    class(value_t), intent(inout) :: val\n    select type(val)\n    type is (leaf_t)\n      self%seen = val%payload\n    class default\n      self%seen = -2\n    end select\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use base_m, only : leaf_t\n  use impl_m, only : counting_visitor_t\n  implicit none\n  type(leaf_t) :: leaf\n  type(counting_visitor_t) :: visitor\n  call leaf%accept(visitor)\n  if (visitor%seen /= 17) error stop 1\n  print *, 'ok'\nend program\n",
    );

    let base_obj = dir.join("base_m.o");
    let impl_obj = dir.join("impl_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("cross_tu_polymorphic_name_bound_dispatch.bin");

    for (src, obj, needs_i) in [
        (&base_src, &base_obj, false),
        (&impl_src, &impl_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "cross-tu polymorphic name dispatch compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            base_obj.to_str().unwrap(),
            impl_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("cross-tu polymorphic name dispatch link spawn failed");
    assert!(
        link.status.success(),
        "cross-tu polymorphic name dispatch should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("cross-tu polymorphic name dispatch run failed");
    assert!(
        run.status.success(),
        "cross-tu polymorphic name dispatch should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected cross-tu polymorphic name dispatch output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_derived_function_result_with_explicit_return_round_trips_through_amod_and_runs() {
    let dir = unique_dir("derived_result_return_amod");
    let mod_src = write_program_in(
        &dir,
        "sugg.f90",
        "module sugg\n  implicit none\n  type, public :: result_t\n    character(len=16) :: text = ''\n    integer :: length = 0\n  end type\ncontains\n  function make_result(flag) result(res)\n    integer, intent(in) :: flag\n    type(result_t) :: res\n    if (flag == 0) return\n    res%text = 'exit'\n    res%length = 4\n    return\n  end function make_result\nend module sugg\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use sugg, only: result_t, make_result\n  implicit none\n  type(result_t) :: res\n  res = make_result(1)\n  if (trim(res%text) /= 'exit') error stop 1\n  if (res%length /= 4) error stop 2\n  print *, trim(res%text), res%length\nend program\n",
    );

    let mod_obj = dir.join("sugg.o");
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
        "main compile failed: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("derived_result_return.bin");
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
        "link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run spawn failed");
    assert!(
        run.status.success(),
        "program run failed: stdout={} stderr={}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("exit") && stdout.contains("4"),
        "expected explicit-return derived result to survive import and runtime: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn typed_header_derived_function_result_uses_hidden_result_abi() {
    let src = write_program(
        "module m\n  implicit none\n  type :: vec\n    real :: x\n  end type\ncontains\n  type(vec) function add_vec(a, b)\n    type(vec), intent(in) :: a, b\n    add_vec%x = a%x + b%x\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  type(vec) :: a, b, r\n  a%x = 1.0\n  b%x = 2.0\n  r = add_vec(a, b)\n  if (abs(r%x - 3.0) > 1.0e-6) error stop 1\n  print *, r%x\nend program\n",
        "f90",
    );
    let out = unique_path("typed_header_derived_result", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("typed-header derived result compile failed to spawn");
    assert!(
        compile.status.success(),
        "typed-header derived result compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("typed-header derived result run failed");
    assert!(
        run.status.success(),
        "typed-header derived result run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("3.0000000E0"),
        "unexpected typed-header derived result output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_type_finalizer_round_trips_through_amod_and_runs() {
    let dir = unique_dir("finalizer_amod");
    let mod_src = write_program_in(
        &dir,
        "m.f90",
        "module m\n  implicit none\n  integer :: hits = 0\n  type, public :: box_t\n  contains\n    final :: destroy_box\n  end type\ncontains\n  subroutine destroy_box(self)\n    type(box_t), intent(inout) :: self\n    hits = hits + 1\n  end subroutine\n\n  integer function get_hits()\n    get_hits = hits\n  end function\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use m, only: box_t, get_hits\n  implicit none\n  block\n    type(box_t) :: value\n  end block\n  if (get_hits() /= 1) error stop 1\n  print *, get_hits()\nend program\n",
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
        "module with finalizer should compile: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let amod = dir.join("m.amod");
    let amod_text = std::fs::read_to_string(&amod).expect("missing m.amod");
    assert!(
        amod_text.contains("@final afs_modproc_m_destroy_box"),
        "module finalizer should round-trip with ABI-qualified name: {}",
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
        "imported type with finalizer should compile: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("finalizer_amod.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            mod_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("finalizer link spawn failed");
    assert!(
        link.status.success(),
        "imported type finalizer objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("finalizer run failed");
    assert!(
        run.status.success(),
        "imported type finalizer should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("1"),
        "imported finalizer should increment module state: {}",
        String::from_utf8_lossy(&run.stdout)
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
fn any_on_vector_subscripted_char_array_compare_runs() {
    let src = write_program(
        "program p\n  implicit none\n  character(1) :: s(10)\n  logical :: ok\n  s = 'x'\n  s(5) = '-'\n  s(8) = '-'\n  ok = .not. any(s([5, 8]) /= '-')\n  if (.not. ok) error stop 1\n  print *, ok\nend program\n",
        "f90",
    );
    let out = unique_path("any_vector_subscript_char_compare", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("vector subscript ANY compile failed to spawn");
    assert!(
        compile.status.success(),
        "vector subscript ANY compare should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("vector subscript ANY run failed");
    assert!(
        run.status.success(),
        "vector subscript ANY compare should run: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout).to_lowercase();
    assert!(
        stdout.contains('t'),
        "expected vector subscript ANY compare to stay true, got: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
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
fn module_procedure_pointer_call_target_is_retained_for_linking() {
    let src = write_program(
        "module m\n  implicit none\n  abstract interface\n    subroutine hook_iface(msg)\n      character(len=*), intent(in) :: msg\n    end subroutine hook_iface\n  end interface\n  procedure(hook_iface), pointer :: hook => null()\ncontains\n  subroutine init()\n    hook => impl\n  end subroutine init\n\n  subroutine run(msg)\n    character(len=*), intent(in) :: msg\n    call hook(msg)\n  end subroutine run\n\n  subroutine impl(msg)\n    character(len=*), intent(in) :: msg\n    if (trim(msg) /= 'ok') error stop 1\n    print *, trim(msg)\n  end subroutine impl\nend module\n\nprogram p\n  use m\n  implicit none\n  call init()\n  call run('ok')\nend program\n",
        "f90",
    );
    let out = unique_path("module_proc_ptr_call_target", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("module procedure pointer call-target compile failed to spawn");
    assert!(
        compile.status.success(),
        "module procedure pointer call target should compile and link: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("module procedure pointer call-target run failed");
    assert!(
        run.status.success(),
        "module procedure pointer call target should run: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "module procedure pointer call target should preserve the procedure-pointer global: {}",
        stdout
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
fn imported_module_c_funptr_global_passes_value_not_storage_address() {
    let dir = unique_dir("amod_imported_c_funptr_global");
    let c_src = write_program_in(
        &dir,
        "handler_matches.c",
        "#include <stdint.h>\n\nint handler_matches(int sig, void *handler, intptr_t expected) {\n    (void)sig;\n    return (intptr_t)handler == expected;\n}\n",
    );
    let c_obj = dir.join("handler_matches.o");
    compile_c_object(&c_src, &c_obj);

    let mod_src = write_program_in(
        &dir,
        "sysint.f90",
        "module sysint\n  use iso_c_binding\n  implicit none\n  type(c_funptr) :: sig_dfl, sig_ign\ncontains\n  subroutine init_consts()\n    sig_dfl = c_null_funptr\n    sig_ign = transfer(1_c_intptr_t, sig_ign)\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_int, c_funptr, c_intptr_t\n  use sysint, only: init_consts, sig_dfl, sig_ign\n  implicit none\n  interface\n    function handler_matches(sig, handler, expected) bind(C, name='handler_matches') result(rc)\n      import :: c_int, c_funptr, c_intptr_t\n      integer(c_int), value :: sig\n      type(c_funptr), value :: handler\n      integer(c_intptr_t), value :: expected\n      integer(c_int) :: rc\n    end function\n  end interface\n  integer(c_int) :: rc\n\n  call init_consts()\n  rc = handler_matches(2_c_int, sig_dfl, 0_c_intptr_t)\n  if (rc /= 1_c_int) error stop 1\n  rc = handler_matches(3_c_int, sig_ign, 1_c_intptr_t)\n  if (rc /= 1_c_int) error stop 2\n  print *, 'ok'\nend program\n",
    );

    let mod_obj = dir.join("sysint.o");
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
        .expect("c_funptr module compile failed to spawn");
    assert!(
        compile_mod.status.success(),
        "c_funptr module should compile: {}",
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
        .expect("c_funptr user compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "c_funptr user should compile: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("amod_imported_c_funptr_global.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            main_obj.to_str().unwrap(),
            mod_obj.to_str().unwrap(),
            c_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("c_funptr user link failed to spawn");
    assert!(
        link.status.success(),
        "c_funptr user objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("c_funptr user run failed");
    assert!(
        run.status.success(),
        "imported module c_funptr globals should pass raw function-pointer values: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "imported module c_funptr globals should preserve the stored scalar values across .amod import: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
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
fn fixed_char_array_constructor_assignment_preserves_runtime_bytes() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=20) :: builtins(3)\n  builtins = [ 'cd                  ', 'echo                ', 'printf              ' ]\n  print '(a)', trim(builtins(1))\n  print '(a)', trim(builtins(2))\n  print '(a)', trim(builtins(3))\nend program\n",
        "f90",
    );
    let out = unique_path("char_array_ctor_assign", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("char array constructor assignment compile spawn failed");
    assert!(
        compile.status.success(),
        "char array constructor assignment compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    assert!(
        !String::from_utf8_lossy(&compile.stderr).contains("unhandled coercion"),
        "char array constructor assignment should not hit generic coercion fallback: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "char array constructor assignment runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    let lines: Vec<_> = stdout.lines().map(str::trim).collect();
    assert_eq!(
        lines,
        vec!["cd", "echo", "printf"],
        "unexpected fixed char array constructor assignment output"
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
fn inquire_unit_size_widens_through_runtime_and_stores_back() {
    let dir = unique_dir("inquire_unit_size");
    let input = dir.join("payload.txt");
    fs::write(&input, "hello").expect("write payload");
    let src = write_program_in(
        &dir,
        "main.f90",
        &format!(
            "program p\n  implicit none\n  integer :: unit, ios, file_size\n  unit = -1\n  ios = -1\n  file_size = -1\n  open(newunit=unit, file='{}', status='old', access='stream', form='unformatted', action='read', iostat=ios)\n  if (ios /= 0) error stop 1\n  inquire(unit=unit, size=file_size)\n  if (file_size /= 5) error stop 2\n  close(unit)\n  print *, 'ok'\nend program\n",
            input.display()
        ),
    );
    let out = dir.join("main.out");
    let compile = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "INQUIRE(unit=, size=) compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "INQUIRE(unit=, size=) runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected INQUIRE(unit=, size=) output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
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
fn repeat_left_concat_assignment_preserves_leading_bytes() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=16) :: s\n  s = repeat(' ', 8) // 'hi'\n  print '(a)', '<' // s(1:10) // '>'\n  s = repeat('0', 3) // '42'\n  print '(a)', '<' // s(1:5) // '>'\nend program\n",
        "f90",
    );
    let out = unique_path("repeat_left_concat", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "repeat-left concat compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "repeat-left concat runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("<        hi>") && stdout.contains("<00042>"),
        "repeat-left concat should preserve leading spaces and zeros: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn internal_read_err_label_success_path_preserves_substring_store() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: pos, format_len, output_pos, octal_val, i\n  character(len=8) :: format_str\n  character(len=8) :: output\n  character :: escape_char\n  character(len=3) :: octal_str\n\n  format_str = '\\101'\n  output = ''\n  pos = 1\n  output_pos = 1\n  format_len = len_trim(format_str)\n\n  pos = pos + 1\n  escape_char = format_str(pos:pos)\n  select case (escape_char)\n  case ('0', '1', '2', '3', '4', '5', '6', '7')\n    octal_str = escape_char\n    do i = 2, 3\n      if (pos + i - 1 <= format_len) then\n        escape_char = format_str(pos + i - 1:pos + i - 1)\n        if (escape_char >= '0' .and. escape_char <= '7') then\n          octal_str(i:i) = escape_char\n        else\n          exit\n        end if\n      else\n        exit\n      end if\n    end do\n    read(octal_str, '(O3)', err=50) octal_val\n    output(output_pos:output_pos) = char(mod(octal_val, 256))\n    pos = pos + len_trim(octal_str) - 1\n    goto 60\n50  output(output_pos:output_pos) = format_str(pos:pos)\n60  continue\n  end select\n\n  if (output(1:1) /= 'A') error stop 1\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("internal_read_err_label_success", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "internal READ ERR= success-path compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "internal READ ERR= success-path runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "internal READ ERR= success-path should preserve the success result: {}",
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
fn char_parameter_concat_with_intrinsic_char_emits_runtime_bytes() {
    let src = write_program(
        "module colors\n  implicit none\n  character(len=*), parameter :: color_match = char(27) // '[01;31m'\n  character(len=*), parameter :: color_reset = char(27) // '[0m'\ncontains\n  subroutine check()\n    if (len(color_match) /= 8) error stop 1\n    if (iachar(color_match(1:1)) /= 27) error stop 2\n    if (color_match(2:8) /= '[01;31m') error stop 3\n    if (len(color_reset) /= 4) error stop 4\n    if (iachar(color_reset(1:1)) /= 27) error stop 5\n    if (color_reset(2:4) /= '[0m') error stop 6\n    write(*, '(a)') color_match // 'module' // color_reset\n  end subroutine\nend module\nprogram p\n  use colors\n  implicit none\n  call check()\nend program\n",
        "char_param_intrinsic_concat.f90",
    );
    let out = unique_path("char_param_intrinsic_concat", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("char intrinsic concat compile spawn failed");
    assert!(
        compile.status.success(),
        "char intrinsic concat program should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("char intrinsic concat run failed");
    assert!(
        run.status.success(),
        "char intrinsic concat program should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout),
        "\u{1b}[01;31mmodule\u{1b}[0m\n",
        "unexpected char intrinsic concat runtime output"
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn reexported_c_null_char_round_trips_through_amod_import() {
    let dir = unique_dir("reexported_c_null_char_amod");
    let wrapper_src = write_program_in(
        &dir,
        "wrapper.f90",
        "module wrapper\n  use iso_c_binding\n  implicit none\ncontains\n  subroutine touch(buf)\n    character(kind=c_char), intent(out) :: buf\n    buf = c_null_char\n  end subroutine\nend module\n",
    );
    let user_src = write_program_in(
        &dir,
        "user.f90",
        "program p\n  use wrapper, only: c_null_char\n  implicit none\n  character(len=:), allocatable :: path\n  path = 'abc' // c_null_char\n  if (len(path) /= 4) error stop 1\n  if (iachar(path(4:4)) /= 0) error stop 2\n  print '(a,i0,a,i0)', 'LEN=', len(path), ' LAST=', iachar(path(4:4))\nend program\n",
    );
    let wrapper_obj = dir.join("wrapper.o");
    let user_obj = dir.join("user.o");
    let out = dir.join("user_bin");

    let compile_wrapper = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            wrapper_src.to_str().unwrap(),
            "-o",
            wrapper_obj.to_str().unwrap(),
        ])
        .output()
        .expect("reexported c_null_char wrapper compile spawn failed");
    assert!(
        compile_wrapper.status.success(),
        "reexported c_null_char wrapper compile failed: {}",
        String::from_utf8_lossy(&compile_wrapper.stderr)
    );

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
        .expect("reexported c_null_char user compile spawn failed");
    assert!(
        compile_user.status.success(),
        "reexported c_null_char user compile failed: {}",
        String::from_utf8_lossy(&compile_user.stderr)
    );

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            wrapper_obj.to_str().unwrap(),
            user_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("reexported c_null_char link spawn failed");
    assert!(
        link.status.success(),
        "reexported c_null_char link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "reexported c_null_char runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("LEN=")
            && stdout.contains('4')
            && stdout.contains("LAST=")
            && stdout.contains('0'),
        "unexpected reexported c_null_char runtime output: {}",
        stdout
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
fn public_defined_assignment_in_private_module_round_trips_through_amod_and_runs() {
    let dir = unique_dir("public_defined_assignment_amod");
    let mod_src = write_program_in(
        &dir,
        "box_mod.f90",
        "module box_mod\n  implicit none\n  private\n  public :: box_t, assignment(=)\n  type :: box_t\n    integer :: value = 0\n  end type\n  interface assignment(=)\n    module procedure assign_box_from_int\n  end interface\ncontains\n  subroutine assign_box_from_int(lhs, rhs)\n    type(box_t), intent(out) :: lhs\n    integer, intent(in) :: rhs\n    lhs%value = rhs * 2\n  end subroutine\nend module\n",
    );
    let user_src = write_program_in(
        &dir,
        "main.f90",
        "program main\n  use box_mod\n  implicit none\n  type(box_t) :: box\n  box = 21\n  print *, box%value\nend program\n",
    );

    let mod_obj = dir.join("box_mod.o");
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
        .expect("defined assignment module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "defined assignment module should compile: {}",
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
        .expect("defined assignment user compile spawn failed");
    assert!(
        compile_user.status.success(),
        "defined assignment consumer should compile through .amod: {}",
        String::from_utf8_lossy(&compile_user.stderr)
    );

    let exe = dir.join("public_defined_assignment_amod.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            mod_obj.to_str().unwrap(),
            user_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("defined assignment link spawn failed");
    assert!(
        link.status.success(),
        "defined assignment .amod link should succeed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run failed");
    assert!(
        run.status.success(),
        "defined assignment runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert_eq!(
        String::from_utf8_lossy(&run.stdout).trim(),
        "42",
        "public defined assignment should stay callable across .amod import"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_fixed_logical_component_array_whole_assignment_clears_all_elements() {
    let dir = unique_dir("imported_fixed_logical_component_clear");
    let mod_src = write_program_in(
        &dir,
        "types_mod.f90",
        "module types_mod\n  implicit none\n  type :: token_t\n    logical :: char_class(0:255) = .true.\n  end type\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program main\n  use types_mod\n  implicit none\n  type(token_t) :: tok\n  tok%char_class = .false.\n  tok%char_class(iachar('a')) = .true.\n  tok%char_class(iachar('b')) = .true.\n  tok%char_class(iachar('c')) = .true.\n  if (.not. tok%char_class(iachar('a'))) error stop 1\n  if (tok%char_class(iachar('z'))) error stop 2\n  print *, 'ok'\nend program\n",
    );

    let mod_obj = dir.join("types_mod.o");
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
        .expect("logical component module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "logical component module should compile: {}",
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
        .expect("logical component consumer compile spawn failed");
    assert!(
        compile_main.status.success(),
        "logical component consumer should compile through .amod: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("logical_component_clear.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            mod_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("logical component link spawn failed");
    assert!(
        link.status.success(),
        "logical component .amod link should succeed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("logical component run failed");
    assert!(
        run.status.success(),
        "logical component runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "logical component whole-array assignment should clear the imported component storage: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn ichar_and_iachar_treat_high_bit_bytes_as_unsigned() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=1) :: c\n  c = achar(255)\n  if (ichar(c) /= 255) error stop 1\n  if (iachar(c) /= 255) error stop 2\n  print *, ichar(c), iachar(c)\nend program\n",
        "f90",
    );
    let out = unique_path("ichar_unsigned_high_bit", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("ichar unsigned compile failed to spawn");
    assert!(
        compile.status.success(),
        "ichar unsigned compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("ichar unsigned run failed");
    assert!(
        run.status.success(),
        "ichar unsigned runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("255"),
        "ichar/iachar should preserve unsigned byte values: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_public_type_bound_private_target_preserves_absent_optional_slots() {
    let dir = unique_dir("imported_type_bound_private_target_optional");
    let mod_src = dir.join("m.f90");
    let main_src = dir.join("p.f90");
    std::fs::write(
        &mod_src,
        "module m\n  implicit none\n  private\n  public :: list_t\n  type :: list_t\n    integer :: x = 0\n  contains\n    procedure :: init => token_list_init\n  end type\ncontains\n  subroutine token_list_init(this, n)\n    class(list_t), intent(inout) :: this\n    integer, intent(in), optional :: n\n    if (present(n)) then\n      this%x = n\n    else\n      this%x = 42\n    end if\n  end subroutine\nend module\n",
    )
    .expect("write module");
    std::fs::write(
        &main_src,
        "program p\n  use m\n  implicit none\n  type(list_t) :: v\n  call v%init()\n  if (v%x /= 42) error stop 1\n  print *, v%x\nend program\n",
    )
    .expect("write program");

    let mod_obj = dir.join("m.o");
    let module_build = Command::new(compiler("armfortas"))
        .args([
            "-c",
            mod_src.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        module_build.status.success(),
        "module build should succeed: {}",
        String::from_utf8_lossy(&module_build.stderr)
    );

    let main_obj = dir.join("p.o");
    let main_compile = Command::new(compiler("armfortas"))
        .args([
            "-c",
            main_src.to_str().unwrap(),
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        main_compile.status.success(),
        "main compile should succeed: {}",
        String::from_utf8_lossy(&main_compile.stderr)
    );

    let out = dir.join("p.bin");
    let link = Command::new(compiler("armfortas"))
        .args([
            main_obj.to_str().unwrap(),
            mod_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        link.status.success(),
        "link should succeed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "imported private target type-bound optional repro should run:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("42"),
        "expected imported private target absent optional on type-bound call to arrive as not-present, got: {}",
        stdout
    );
}

#[test]
fn imported_type_bound_function_returning_derived_value_round_trips_through_private_target() {
    let dir = unique_dir("imported_type_bound_private_result");
    let mod_src = dir.join("m.f90");
    let main_src = dir.join("p.f90");
    std::fs::write(
        &mod_src,
        "module m\n  implicit none\n  private\n  public :: token_t, list_t\n  type :: token_t\n    integer :: value = 0\n  end type\n  type :: list_t\n  contains\n    procedure :: get => token_list_get\n  end type\ncontains\n  function token_list_get(this, idx) result(tok)\n    class(list_t), intent(in) :: this\n    integer, intent(in) :: idx\n    type(token_t) :: tok\n    tok%value = idx + 40\n  end function\nend module\n",
    )
    .expect("write module");
    std::fs::write(
        &main_src,
        "program p\n  use m\n  implicit none\n  type(list_t) :: v\n  type(token_t) :: tok\n  tok = v%get(2)\n  if (tok%value /= 42) error stop 1\n  if (v%get(3)%value /= 43) error stop 2\n  print *, tok%value, v%get(3)%value\nend program\n",
    )
    .expect("write program");

    let mod_obj = dir.join("m.o");
    let module_build = Command::new(compiler("armfortas"))
        .args([
            "-c",
            mod_src.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        module_build.status.success(),
        "module build should succeed: {}",
        String::from_utf8_lossy(&module_build.stderr)
    );

    let main_obj = dir.join("p.o");
    let main_compile = Command::new(compiler("armfortas"))
        .args([
            "-c",
            main_src.to_str().unwrap(),
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        main_compile.status.success(),
        "main compile should succeed: {}",
        String::from_utf8_lossy(&main_compile.stderr)
    );

    let out = dir.join("p.bin");
    let link = Command::new(compiler("armfortas"))
        .args([
            main_obj.to_str().unwrap(),
            mod_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        link.status.success(),
        "link should succeed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "imported private target type-bound function result repro should run:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("42") && stdout.contains("43"),
        "expected imported type-bound derived results to survive private target round-trip, got: {}",
        stdout
    );
}

#[test]
fn enum_bind_c_enumerators_compile_and_run() {
    let src = write_program(
        "module colors\n  implicit none\n  enum, bind(c)\n    enumerator :: red = 1, blue = 2, green = 3\n  end enum\nend module\nprogram main\n  use colors\n  implicit none\n  integer, parameter :: color_kind = kind(red)\n  print *, red, blue, green, color_kind\nend program\n",
        "f90",
    );
    let out = unique_path("enum_bind_c", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("enum bind(c) compile spawn failed");
    assert!(
        compile.status.success(),
        "enum bind(c) program should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "enum bind(c) runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains('1')
            && stdout.contains('2')
            && stdout.contains('3')
            && (stdout.contains(" 4") || stdout.contains("\n4") || stdout.contains(" 8")),
        "enum bind(c) enumerators should behave like integer parameters: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn complex_intrinsics_preserve_complex_storage_without_i128_backend_fallback() {
    let src = write_program(
        r#"
module complex_box_mod
  use iso_fortran_env, only: real64
  implicit none
  type :: complex_box_t
    complex(real64) :: z = (0.0_real64, 0.0_real64)
    complex(real64), allocatable :: mat(:,:)
  end type
contains
  function build_box(real_part, imag_part) result(box)
    real(real64), intent(in) :: real_part, imag_part
    type(complex_box_t) :: box
    box%z = cmplx(real_part, imag_part, kind=real64)
    allocate(box%mat(1,1))
    box%mat(1,1) = conjg(box%z)
  end function
end module

program main
  use iso_fortran_env, only: real64
  use complex_box_mod
  implicit none
  type(complex_box_t) :: box
  box = build_box(1.5_real64, -2.5_real64)
  if (abs(real(box%z) - 1.5_real64) > 1.0e-12_real64) error stop 1
  if (abs(aimag(box%z) + 2.5_real64) > 1.0e-12_real64) error stop 2
  if (abs(real(box%mat(1,1)) - 1.5_real64) > 1.0e-12_real64) error stop 3
  if (abs(aimag(box%mat(1,1)) - 2.5_real64) > 1.0e-12_real64) error stop 4
  print *, 'ok'
end program
"#,
        "f90",
    );
    let out = unique_path("complex_intrinsics_no_i128", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args(["-O2", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("complex intrinsic compile spawn failed");
    assert!(
        compile.status.success(),
        "complex intrinsic program should compile cleanly: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "complex intrinsic runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "expected complex intrinsic smoke output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn real_intrinsic_preserves_complex_kind_without_explicit_kind_argument() {
    let src = write_program(
        r#"
module m
  use iso_fortran_env, only: real64
contains
  function phase(z) result(angle)
    complex(real64), intent(in) :: z
    real(real64) :: angle
    angle = atan2(aimag(z), real(z))
  end function
end module

program main
  use iso_fortran_env, only: real64
  use m
  implicit none
  complex(real64) :: z
  real(real64) :: x, angle
  z = cmplx(1.0_real64, 1.0_real64, kind=real64)
  x = real(z)
  if (abs(x - 1.0_real64) > 1.0e-12_real64) error stop 1
  angle = phase(z)
  if (abs(angle - atan(1.0_real64)) > 1.0e-12_real64) error stop 2
  print *, 'ok'
end program
"#,
        "f90",
    );
    let out = unique_path("real_complex_kind_no_explicit_kind", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args(["-O2", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("real complex kind compile spawn failed");
    assert!(
        compile.status.success(),
        "real complex kind compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("real complex kind run failed");
    assert!(
        run.status.success(),
        "real complex kind run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("ok"), "unexpected stdout: {}", stdout);

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn mixed_scalar_complex_division_compiles_and_runs() {
    let src = write_program(
        r#"
program main
  use iso_fortran_env, only: real64
  implicit none
  complex(real64) :: z, got

  z = cmplx(1.0_real64, -1.0_real64, kind=real64)
  got = 6.0_real64 / z

  if (abs(real(got) - 3.0_real64) > 1.0e-12_real64) error stop 1
  if (abs(aimag(got) - 3.0_real64) > 1.0e-12_real64) error stop 2
  print *, 'ok'
end program
"#,
        "f90",
    );
    let out = unique_path("mixed_scalar_complex_div", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("mixed scalar/complex division compile spawn failed");
    assert!(
        compile.status.success(),
        "mixed scalar/complex division should compile cleanly: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "mixed scalar/complex division runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "expected mixed scalar/complex division smoke output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn extended_math_intrinsics_compile_and_run() {
    let src = write_program(
        r#"
program main
  use iso_fortran_env, only: real64
  implicit none
  real(real64) :: x, y
  x = 0.5_real64
  y = 2.0_real64
  if (sinh(x) <= x) error stop 1
  if (cosh(x) <= 1.0_real64) error stop 2
  if (tanh(x) <= 0.0_real64 .or. tanh(x) >= 1.0_real64) error stop 3
  if (asinh(1.25_real64) <= 0.0_real64) error stop 4
  if (acosh(y) <= 0.0_real64) error stop 5
  if (atanh(x) <= 0.0_real64) error stop 6
  if (gamma(5.0_real64) <= 20.0_real64) error stop 7
  if (log_gamma(5.0_real64) <= 3.0_real64) error stop 8
  if (erf(x) <= 0.0_real64) error stop 9
  if (erfc(x) <= 0.0_real64 .or. erfc(x) >= 1.0_real64) error stop 10
  print *, 'ok'
end program
"#,
        "f90",
    );
    let out = unique_path("extended_math_intrinsics", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args(["-O2", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("extended math intrinsic compile spawn failed");
    assert!(
        compile.status.success(),
        "extended math intrinsic program should compile cleanly: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "extended math intrinsic runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "expected extended math intrinsic smoke output: {}",
        String::from_utf8_lossy(&run.stdout)
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
fn allocatable_scalar_derived_intent_out_component_store_runs() {
    let src = write_program(
        "module m\n  implicit none\n  type :: err_t\n    integer :: code = 0\n  end type\ncontains\n  subroutine fill(err)\n    type(err_t), allocatable, intent(out) :: err\n    allocate(err)\n    err%code = 7\n  end subroutine\nend module\n\nprogram p\n  use m\n  implicit none\n  type(err_t), allocatable :: err\n  call fill(err)\n  if (.not. allocated(err)) error stop 1\n  if (err%code /= 7) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("alloc_scalar_derived_out_component_store", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable scalar derived intent(out) compile spawn failed");
    assert!(
        compile.status.success(),
        "allocatable scalar derived intent(out) should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "allocatable scalar derived intent(out) should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected allocatable scalar derived intent(out) output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn optional_logical_present_guard_does_not_deref_absent_dummy() {
    let src = write_program(
        "program p\n  implicit none\n  call s()\n  print *, 'ok'\ncontains\n  subroutine s(rel)\n    logical, intent(in), optional :: rel\n    logical :: relative\n    if (present(rel)) then\n      relative = rel\n    else\n      relative = .false.\n    end if\n    if (relative) error stop 1\n  end subroutine s\nend program p\n",
        "f90",
    );
    let out = unique_path("optional_logical_present_guard", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("optional logical present guard compile spawn failed");
    assert!(
        compile.status.success(),
        "optional logical present guard should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "optional logical present guard should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected optional logical present guard output: {}",
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
fn allocatable_char_component_store_accepts_component_subscript_expr() {
    let src = write_program(
        "module hist_mod\n  implicit none\n  integer, parameter :: max_line_len = 1024\n  integer, parameter :: max_history = 100\n  type :: history_t\n    character(len=max_line_len), allocatable :: lines(:)\n    integer :: count = 0\n    integer :: current = 0\n    logical :: initialized = .false.\n  end type\n  type(history_t), save :: history\ncontains\n  subroutine init_history()\n    if (.not. history%initialized) then\n      allocate(history%lines(max_history))\n      history%lines = ''\n      history%count = 0\n      history%current = 0\n      history%initialized = .true.\n    end if\n  end subroutine\n\n  subroutine fill()\n    character(len=max_line_len) :: line\n    call init_history()\n    line = 'abc'\n    history%count = 0\n    history%current = 0\n    history%count = history%count + 1\n    history%lines(history%count) = line\n    print *, history%count\n    print *, trim(history%lines(1))\n  end subroutine\nend module\nprogram p\n  use hist_mod\n  call fill()\nend program\n",
        "f90",
    );
    let out = unique_path("alloc_char_component_subscript_expr", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable char component subscript compile spawn failed");
    assert!(
        compile.status.success(),
        "allocatable char component subscript should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "allocatable char component subscript should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("1") && stdout.contains("abc"),
        "unexpected allocatable char component subscript output: {}",
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
fn pointer_function_result_forwarding_preserves_target_association() {
    let src = write_program(
        "module m\n  implicit none\n  type :: node_t\n    integer :: value = 0\n  end type node_t\n  type(node_t), target, save :: pool\ncontains\n  subroutine init_pool(n)\n    integer, intent(in) :: n\n    pool%value = n\n  end subroutine init_pool\n\n  function leaf() result(node)\n    type(node_t), pointer :: node\n    node => pool\n  end function leaf\n\n  function forward() result(node)\n    type(node_t), pointer :: node\n    type(node_t), pointer :: tmp\n    node => leaf()\n    tmp => leaf()\n    if (.not. associated(tmp, pool)) error stop 3\n  end function forward\nend module m\n\nprogram p\n  use m\n  implicit none\n  type(node_t), pointer :: root\n  call init_pool(7)\n  root => forward()\n  if (.not. associated(root, pool)) error stop 1\n  root%value = 42\n  if (pool%value /= 42) error stop 2\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("pointer_result_forwarding_assoc", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("pointer result forwarding association compile spawn failed");
    assert!(
        compile.status.success(),
        "pointer result forwarding association should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "pointer result forwarding association should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected pointer result forwarding association output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn select_type_polymorphic_dummy_dispatches_via_descriptor_tag() {
    let src = write_program(
        "module m\n  implicit none\n  type, abstract :: base_t\n  end type base_t\n  type, extends(base_t) :: float_box\n    real(8) :: raw = 0.0d0\n  end type float_box\ncontains\n  function cast_float(val) result(ptr)\n    class(base_t), intent(in), target :: val\n    real(8), pointer :: ptr\n    select type (val)\n    type is (float_box)\n      ptr => val%raw\n    class default\n      nullify(ptr)\n    end select\n  end function cast_float\nend module m\n\nprogram p\n  use m\n  implicit none\n  type(float_box), target :: box\n  real(8), pointer :: ptr\n  box%raw = 3.5d0\n  ptr => cast_float(box)\n  if (.not. associated(ptr)) error stop 1\n  if (abs(ptr - 3.5d0) > 1.0d-12) error stop 2\n  ptr = 9.25d0\n  if (abs(box%raw - 9.25d0) > 1.0d-12) error stop 3\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("select_type_polymorphic_dummy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("select type pointer result compile spawn failed");
    assert!(
        compile.status.success(),
        "polymorphic SELECT TYPE dummy should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "polymorphic SELECT TYPE dummy runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "expected polymorphic SELECT TYPE dummy to bind the concrete guard, got: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn select_type_pointer_polymorphic_local_dispatches_via_descriptor_tag() {
    let src = write_program(
        "program p\n  implicit none\n  type, abstract :: base_t\n  end type\n  type, extends(base_t) :: child_t\n    integer :: x = 0\n  end type\n  class(base_t), pointer :: tmp\n  type(child_t), target :: child\n  integer :: seen\n  nullify(tmp)\n  child%x = 17\n  tmp => child\n  seen = 0\n  select type (tmp)\n  type is (child_t)\n    seen = tmp%x\n  class default\n    error stop 1\n  end select\n  if (seen /= 17) error stop 2\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("select_type_pointer_poly_local", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("pointer polymorphic select type compile spawn failed");
    assert!(
        compile.status.success(),
        "pointer polymorphic local SELECT TYPE should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "pointer polymorphic local SELECT TYPE runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "expected pointer polymorphic local SELECT TYPE to bind the concrete guard, got: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn select_type_allocatable_polymorphic_component_dispatches_via_descriptor_tag() {
    let src = write_program(
        "program p\n  implicit none\n  type, abstract :: base_t\n  end type\n  type, extends(base_t) :: child_t\n    integer :: x = 0\n  end type\n  type :: holder_t\n    class(base_t), allocatable :: val\n  end type\n  type(holder_t) :: holder\n  integer :: seen\n  allocate(child_t :: holder%val)\n  select type (val => holder%val)\n  type is (child_t)\n    val%x = 23\n  class default\n    error stop 1\n  end select\n  seen = -1\n  select type (val => holder%val)\n  type is (child_t)\n    seen = val%x\n  class default\n    error stop 2\n  end select\n  if (seen /= 23) error stop 3\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("select_type_alloc_poly_component", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable polymorphic component select type compile spawn failed");
    assert!(
        compile.status.success(),
        "allocatable polymorphic component SELECT TYPE should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "allocatable polymorphic component SELECT TYPE runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "expected allocatable polymorphic component SELECT TYPE to bind the concrete guard, got: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_class_dummy_actual_to_concrete_type_dummy_mutates_real_storage() {
    let dir = unique_dir("class_dummy_to_concrete_dummy");
    let type_src = write_program_in(
        &dir,
        "type_m.f90",
        "module type_m\n  implicit none\n  type :: t\n    integer :: x = 0\n  end type\ncontains\n  subroutine init(self)\n    type(t), intent(out) :: self\n    self%x = 42\n  end subroutine\nend module\n",
    );
    let use_src = write_program_in(
        &dir,
        "use_m.f90",
        "module use_m\n  use type_m, only : t, init\n  implicit none\ncontains\n  subroutine run(arg)\n    class(t), intent(inout) :: arg\n    call init(arg)\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use type_m, only : t\n  use use_m, only : run\n  implicit none\n  type(t) :: value\n  call run(value)\n  if (value%x /= 42) error stop 1\n  print *, 'ok'\nend program\n",
    );

    let type_obj = dir.join("type_m.o");
    let use_obj = dir.join("use_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("class_dummy_to_concrete_dummy.bin");

    for (src, obj, needs_i) in [
        (&type_src, &type_obj, false),
        (&use_src, &use_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "class dummy to concrete dummy compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            type_obj.to_str().unwrap(),
            use_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "class dummy to concrete dummy link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run failed");
    assert!(
        run.status.success(),
        "class dummy to concrete dummy runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected class dummy to concrete dummy output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bound_pointer_dummy_prefers_unique_link_name_over_duplicate_scope_name() {
    let dir = unique_dir("bound_pointer_dummy_abi");
    let other_src = write_program_in(
        &dir,
        "other_get_m.f90",
        "module other_get_m\n  implicit none\ncontains\n  subroutine get(x)\n    integer, intent(out) :: x\n    x = 7\n  end subroutine\nend module\n",
    );
    let store_src = write_program_in(
        &dir,
        "store_m.f90",
        "module store_m\n  implicit none\n  type :: base_t\n    integer :: x = 42\n  end type\n  type(base_t), target :: global_value\n  type :: box_t\n  contains\n    procedure :: get\n  end type\ncontains\n  subroutine get(self, ptr)\n    class(box_t), intent(inout) :: self\n    class(base_t), pointer, intent(out) :: ptr\n    nullify(ptr)\n    ptr => global_value\n  end subroutine\nend module\n",
    );
    let use_src = write_program_in(
        &dir,
        "use_m.f90",
        "module use_m\n  use other_get_m, only : get\n  use store_m, only : box_t, base_t\n  implicit none\ncontains\n  subroutine run(ok)\n    logical, intent(out) :: ok\n    type(box_t) :: box\n    class(base_t), pointer :: ptr\n    integer :: scratch\n    call get(scratch)\n    call box%get(ptr)\n    ok = associated(ptr) .and. ptr%x == 42 .and. scratch == 7\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use use_m, only : run\n  implicit none\n  logical :: ok\n  call run(ok)\n  if (.not.ok) error stop 1\n  print *, 'ok'\nend program\n",
    );

    let other_obj = dir.join("other_get_m.o");
    let store_obj = dir.join("store_m.o");
    let use_obj = dir.join("use_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("bound_pointer_dummy_abi.bin");

    for (src, obj, needs_i) in [
        (&other_src, &other_obj, false),
        (&store_src, &store_obj, false),
        (&use_src, &use_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "bound pointer dummy ABI compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            other_obj.to_str().unwrap(),
            store_obj.to_str().unwrap(),
            use_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "bound pointer dummy ABI link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run failed");
    assert!(
        run.status.success(),
        "bound pointer dummy ABI runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected bound pointer dummy ABI output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pointer_dummy_select_type_assignment_updates_caller_slot() {
    let src = write_program(
        "module m\n  implicit none\n  type :: base_t\n    integer :: x = 0\n  end type\n  type, extends(base_t) :: child_t\n  end type\n  type(child_t), target, save :: global_child\ncontains\n  subroutine get_poly(tmp)\n    class(base_t), pointer, intent(out) :: tmp\n    global_child%x = 42\n    tmp => global_child\n  end subroutine\n\n  subroutine narrow(ptr)\n    type(child_t), pointer, intent(out) :: ptr\n    class(base_t), pointer :: tmp\n    nullify(ptr)\n    call get_poly(tmp)\n    if (.not.associated(tmp)) error stop 10\n    select type(tmp)\n    type is(child_t)\n      ptr => tmp\n    class default\n      error stop 11\n    end select\n  end subroutine\nend module\n\nprogram p\n  use m\n  implicit none\n  type(child_t), pointer :: ptr\n  call narrow(ptr)\n  if (.not.associated(ptr)) error stop 1\n  if (ptr%x /= 42) error stop 2\n  print *, 'ok'\nend program\n",
        "pointer_dummy_select_type_assignment_updates_caller_slot",
    );
    let out = src.with_extension("out");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("pointer dummy select type compile failed to spawn");
    assert!(
        compile.status.success(),
        "pointer dummy select type should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("pointer dummy select type run failed to spawn");
    assert!(
        run.status.success(),
        "pointer dummy select type should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected pointer dummy select type output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn select_type_class_default_is_lowered_as_fallback() {
    let src = write_program(
        "program p\n  implicit none\n  type, abstract :: base_t\n  end type\n  type, extends(base_t) :: child_t\n    integer :: x = 0\n  end type\n  type :: holder_t\n    class(base_t), allocatable :: val\n  end type\n  type(holder_t) :: holder\n  integer :: seen\n  allocate(child_t :: holder%val)\n  select type (val => holder%val)\n  class default\n    error stop 1\n  type is (child_t)\n    val%x = 23\n  end select\n  seen = -1\n  select type (val => holder%val)\n  class default\n    error stop 2\n  type is (child_t)\n    seen = val%x\n  end select\n  if (seen /= 23) error stop 3\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("select_type_default_fallback", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("default-first select type compile spawn failed");
    assert!(
        compile.status.success(),
        "default-first SELECT TYPE should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "default-first SELECT TYPE runtime failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "expected default-first SELECT TYPE to reach the matching TYPE IS arm, got: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_amod_type_tags_stay_stable_across_tus() {
    let dir = unique_dir("amod_type_tags_cross_tu");
    let types_src = write_program_in(
        &dir,
        "types_m.f90",
        "module types_m\n  implicit none\n  type, abstract :: base_t\n  end type\n  type, extends(base_t) :: child_t\n    integer :: payload = 0\n  end type\nend module\n",
    );
    let helper_src = write_program_in(
        &dir,
        "helper_m.f90",
        "module helper_m\n  implicit none\n  type :: filler_t\n    integer :: pad = 0\n  end type\nend module\n",
    );
    let producer_src = write_program_in(
        &dir,
        "producer_m.f90",
        "module producer_m\n  use types_m\n  implicit none\ncontains\n  subroutine init(val)\n    class(base_t), allocatable, intent(out) :: val\n    allocate(child_t :: val)\n  end subroutine\nend module\n",
    );
    let consumer_src = write_program_in(
        &dir,
        "consumer.f90",
        "program p\n  use helper_m\n  use types_m\n  use producer_m\n  implicit none\n  class(base_t), allocatable :: val\n  call init(val)\n  select type (val)\n  type is (child_t)\n    print *, 'ok'\n  class default\n    error stop 1\n  end select\nend program\n",
    );

    let types_obj = dir.join("types_m.o");
    let helper_obj = dir.join("helper_m.o");
    let producer_obj = dir.join("producer_m.o");
    let consumer_obj = dir.join("consumer.o");
    let exe = dir.join("consumer.bin");

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
        "types module compile failed: {}",
        String::from_utf8_lossy(&compile_types.stderr)
    );

    let compile_helper = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            helper_src.to_str().unwrap(),
            "-o",
            helper_obj.to_str().unwrap(),
        ])
        .output()
        .expect("helper module compile spawn failed");
    assert!(
        compile_helper.status.success(),
        "helper module compile failed: {}",
        String::from_utf8_lossy(&compile_helper.stderr)
    );

    let compile_producer = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            "-I",
            dir.to_str().unwrap(),
            producer_src.to_str().unwrap(),
            "-o",
            producer_obj.to_str().unwrap(),
        ])
        .output()
        .expect("producer module compile spawn failed");
    assert!(
        compile_producer.status.success(),
        "producer module compile failed: {}",
        String::from_utf8_lossy(&compile_producer.stderr)
    );

    let compile_consumer = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            consumer_src.to_str().unwrap(),
            "-o",
            consumer_obj.to_str().unwrap(),
        ])
        .output()
        .expect("consumer compile spawn failed");
    assert!(
        compile_consumer.status.success(),
        "consumer compile failed: {}",
        String::from_utf8_lossy(&compile_consumer.stderr)
    );

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            consumer_obj.to_str().unwrap(),
            producer_obj.to_str().unwrap(),
            helper_obj.to_str().unwrap(),
            types_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("consumer run failed");
    assert!(
        run.status.success(),
        "cross-TU imported type tags should remain stable at runtime: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected cross-TU imported type tag output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn descriptor_backed_class_pointer_accepts_pointer_function_result_without_verifier_ice() {
    let src = write_program(
        "module m\n  implicit none\n  type, abstract :: base_t\n    integer :: origin = 0\n  end type\n  type, extends(base_t) :: child_t\n    integer :: x = 0\n    integer :: y = 0\n  end type\ncontains\n  function cast_to_child(ptr) result(p)\n    class(base_t), intent(in), target :: ptr\n    type(child_t), pointer :: p\n    nullify(p)\n    select type(ptr)\n    type is (child_t)\n      p => ptr\n    end select\n  end function cast_to_child\nend module\nprogram p\n  use m\n  implicit none\n  class(base_t), pointer :: src\n  class(base_t), allocatable :: tmp\n  class(child_t), pointer :: q\n  type(child_t), target :: child\n  child%origin = 5\n  child%x = 7\n  child%y = 9\n  src => child\n  allocate(tmp, source=src)\n  q => cast_to_child(tmp)\n  if (.not. associated(q)) error stop 1\n  if (q%origin /= 5) error stop 2\n  if (q%x /= 7) error stop 3\n  if (q%y /= 9) error stop 4\n  q%origin = 1\n  q%x = 11\n  if (q%origin /= 1) error stop 5\n  if (q%x /= 11) error stop 6\n  if (q%y /= 9) error stop 7\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("class_ptr_func_result_descriptor", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("descriptor-backed class pointer function result compile spawn failed");
    assert!(
        compile.status.success(),
        "descriptor-backed class pointer function result should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "descriptor-backed class pointer function result should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected descriptor-backed class pointer function result output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn descriptor_backed_class_cast_preserves_object_base_with_unallocated_parent_allocatable() {
    let src = write_program(
        "module m\n  implicit none\n  type, abstract :: base_t\n    character(len=:), allocatable :: key\n    integer :: origin = 0\n  end type\n  type, extends(base_t) :: child_t\n    integer :: x = 0\n    integer :: y = 0\n  end type\ncontains\n  function cast_to_child(ptr) result(p)\n    class(base_t), intent(in), target :: ptr\n    type(child_t), pointer :: p\n    nullify(p)\n    select type(ptr)\n    type is (child_t)\n      p => ptr\n    end select\n  end function cast_to_child\nend module\nprogram p\n  use m\n  implicit none\n  class(base_t), pointer :: src\n  class(base_t), allocatable :: tmp\n  class(child_t), pointer :: q\n  type(child_t), target :: child\n  child%origin = 5\n  child%x = 7\n  child%y = 9\n  src => child\n  allocate(tmp, source=src)\n  q => cast_to_child(tmp)\n  if (.not. associated(q)) error stop 1\n  if (q%origin /= 5) error stop 2\n  if (q%x /= 7) error stop 3\n  if (q%y /= 9) error stop 4\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("class_cast_parent_allocatable_base", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("descriptor-backed class cast compile spawn failed");
    assert!(
        compile.status.success(),
        "descriptor-backed class cast should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "descriptor-backed class cast should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected descriptor-backed class cast output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn polymorphic_allocate_source_uses_concrete_dynamic_layout() {
    let src = write_program(
        "module m\n  implicit none\n  type, abstract :: base_t\n  end type\n  type, extends(base_t) :: child_t\n    integer :: x = 0\n    integer :: extra = 0\n  end type\nend module m\nprogram p\n  use m\n  implicit none\n  class(base_t), pointer :: src\n  class(base_t), allocatable :: tmp\n  type(child_t), target :: child\n  child%x = 7\n  child%extra = 9\n  src => child\n  allocate(tmp, source=src)\n  select type (tmp)\n  type is (child_t)\n    tmp%extra = 11\n    if (tmp%x /= 7) error stop 1\n    if (tmp%extra /= 11) error stop 2\n  class default\n    error stop 3\n  end select\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("polymorphic_allocate_source_dynamic_layout", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("polymorphic allocate source compile spawn failed");
    assert!(
        compile.status.success(),
        "polymorphic allocate source should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "polymorphic allocate source should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected polymorphic allocate source output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn derived_array_element_assignment_deep_copies_allocatable_components() {
    let src = write_program(
        "module m\n  implicit none\n  type :: token_t\n    integer :: token_type = 0\n    character(len=:), allocatable :: text\n    integer :: position = 0\n  end type token_t\ncontains\n  function make_token(ttype, text, pos) result(token)\n    integer, intent(in) :: ttype, pos\n    character(len=*), intent(in) :: text\n    type(token_t) :: token\n    token%token_type = ttype\n    token%text = text\n    token%position = pos\n  end function make_token\nend module m\n\nprogram p\n  use m\n  implicit none\n  type(token_t), allocatable :: tokens(:)\n  type(token_t) :: current\n  allocate(tokens(4))\n  current = make_token(1, '2', 1)\n  tokens(1) = current\n  current = make_token(3, '+', 3)\n  tokens(2) = current\n  current = make_token(1, '3', 5)\n  tokens(3) = current\n  if (tokens(1)%token_type /= 1) error stop 1\n  if (trim(tokens(1)%text) /= '2') error stop 2\n  if (tokens(1)%position /= 1) error stop 3\n  if (tokens(2)%token_type /= 3) error stop 4\n  if (trim(tokens(2)%text) /= '+') error stop 5\n  if (tokens(2)%position /= 3) error stop 6\n  if (tokens(3)%token_type /= 1) error stop 7\n  if (trim(tokens(3)%text) /= '3') error stop 8\n  if (tokens(3)%position /= 5) error stop 9\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("derived_array_element_deep_copy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-O2", "-o", out.to_str().unwrap()])
        .output()
        .expect("derived array element deep copy compile spawn failed");
    assert!(
        compile.status.success(),
        "derived array element deep copy should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "derived array element deep copy should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected derived array element deep copy output: {}",
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
fn allocatable_fixed_char_array_element_substring_assignment_preserves_written_byte() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=8), allocatable :: temp(:)\n  allocate(temp(1))\n  temp(1) = 'hello'\n  temp(1)(6:6) = char(0)\n  if (iachar(temp(1)(6:6)) /= 0) error stop 1\n  print *, iachar(temp(1)(6:6))\nend program\n",
        "f90",
    );
    let out = unique_path("alloc_char_elem_substring", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable char array element substring compile spawn failed");
    assert!(
        compile.status.success(),
        "allocatable char array element substring should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "allocatable char array element substring should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains('0'),
        "unexpected allocatable char array element substring output: {}",
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
fn allocate_pointer_intent_out_dummy_updates_caller_slot_and_components() {
    let src = write_program(
        "program p\n  implicit none\n  type :: node_t\n    integer :: value = 0\n    character(len=16) :: name = ''\n  end type\n  type(node_t), pointer :: root\n  call init(root)\n  if (.not. associated(root)) error stop 1\n  if (root%value /= 7) error stop 2\n  if (trim(root%name) /= 'root') error stop 3\n  print *, 'ok'\ncontains\n  subroutine init(root)\n    type(node_t), pointer, intent(out) :: root\n    allocate(root)\n    root%value = 7\n    root%name = 'root'\n  end subroutine\nend program p\n",
        "f90",
    );
    let out = unique_path("pointer_intent_out_allocate_components", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("pointer intent(out) allocate compile failed to spawn");
    assert!(
        compile.status.success(),
        "pointer intent(out) allocate should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "pointer intent(out) allocate should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected pointer intent(out) allocate output: {}",
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
fn multi_char_array_constructor_actual_to_assumed_shape_dummy_survives_derived_result_assignment() {
    let src = write_program(
        "module repro\n  implicit none\n  type :: command_t\n    character(len=:), allocatable :: argv(:)\n  end type\ncontains\n  function make(argv) result(out)\n    character(len=*), intent(in) :: argv(:)\n    type(command_t) :: out\n    integer :: i, n\n    n = len(argv(1))\n    allocate(character(len=n) :: out%argv(size(argv)))\n    do i = 1, size(argv)\n      out%argv(i) = argv(i)\n    end do\n  end function\nend module\nprogram p\n  use repro\n  implicit none\n  type(command_t) :: cmd\n  cmd = make([\"hello\", \"world\"])\n  if (.not. allocated(cmd%argv)) error stop 1\n  if (size(cmd%argv) /= 2) error stop 2\n  if (cmd%argv(1) /= \"hello\") error stop 3\n  if (cmd%argv(2) /= \"world\") error stop 4\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("multi_char_array_constructor_actual", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("multi-element char array constructor actual compile spawn failed");
    assert!(
        compile.status.success(),
        "multi-element char array constructor actual should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "multi-element char array constructor actual should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected multi-element char array constructor actual output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn derived_result_with_scalar_char_and_constructor_array_actual_survives() {
    let src = write_program(
        "module repro\n  implicit none\n  type :: command_t\n    character(len=:), allocatable :: program\n    character(len=:), allocatable :: argv(:)\n  end type\ncontains\n  function make(program, argv) result(out)\n    character(len=*), intent(in) :: program\n    character(len=*), intent(in) :: argv(:)\n    type(command_t) :: out\n    integer :: i, n\n    out%program = program\n    n = len(argv(1))\n    allocate(character(len=n) :: out%argv(size(argv)))\n    do i = 1, size(argv)\n      out%argv(i) = argv(i)\n    end do\n  end function\nend module\nprogram p\n  use repro\n  implicit none\n  type(command_t) :: cmd\n  cmd = make(\"printf\", [\"hello\", \"world\"])\n  if (.not. allocated(cmd%program)) error stop 1\n  if (cmd%program /= \"printf\") error stop 2\n  if (.not. allocated(cmd%argv)) error stop 3\n  if (size(cmd%argv) /= 2) error stop 4\n  if (cmd%argv(1) /= \"hello\") error stop 5\n  if (cmd%argv(2) /= \"world\") error stop 6\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("derived_result_constructor_actual", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("mixed scalar char and constructor array actual compile spawn failed");
    assert!(
        compile.status.success(),
        "mixed scalar char and constructor array actual should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "mixed scalar char and constructor array actual should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected mixed scalar char and constructor array actual output: {}",
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
fn nested_pooled_char_pointer_component_whole_assign_then_char_store_round_trips() {
    let src = write_program(
        "module string_pool_like\n  implicit none\n  type :: string_ref\n    integer :: pool_index = 0\n    integer :: ref_count = 0\n    integer :: str_len = 0\n    character(:), pointer :: data => null()\n  end type string_ref\n  character(len=32), target :: pool(1)\ncontains\n  function pool_get_string(length) result(ref)\n    integer, intent(in) :: length\n    type(string_ref) :: ref\n    pool = ''\n    ref%str_len = length\n    ref%data => pool(1)(1:length)\n  end function pool_get_string\nend module\nprogram p\n  use string_pool_like\n  implicit none\n  type :: input_state_t\n    type(string_ref) :: buffer_ref\n    integer :: length = 0\n  end type input_state_t\n  type(input_state_t) :: state\n  state%buffer_ref = pool_get_string(32)\n  if (.not. associated(state%buffer_ref%data)) error stop 2\n  state%buffer_ref%data = ''\n  call set_char(state, 1, 'e')\n  call set_char(state, 2, 'c')\n  call set_char(state, 3, 'h')\n  call set_char(state, 4, 'o')\n  state%length = 4\n  if (trim(state%buffer_ref%data) /= 'echo') error stop 1\n  print *, trim(state%buffer_ref%data)\ncontains\n  subroutine set_char(state, pos, ch)\n    type(input_state_t), intent(inout) :: state\n    integer, intent(in) :: pos\n    character(len=1), intent(in) :: ch\n    if (pos >= 1 .and. pos <= len(state%buffer_ref%data)) then\n      state%buffer_ref%data(pos:pos) = ch\n    end if\n  end subroutine set_char\nend program\n",
        "f90",
    );
    let out = unique_path("nested_pooled_char_pointer_component", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("nested pooled char pointer compile failed to spawn");
    assert!(
        compile.status.success(),
        "nested pooled char pointer compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("nested pooled char pointer run failed");
    assert!(
        run.status.success(),
        "nested pooled char pointer run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("echo"),
        "unexpected nested pooled char pointer output: {}",
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
fn get_environment_variable_status_keyword_preserves_length_hole() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=16) :: test_mode\n  integer :: stat_out\n  test_mode = ''\n  stat_out = -1\n  call get_environment_variable('FORTSH_TEST_MODE', test_mode, status=stat_out)\n  if (stat_out /= 0) error stop 1\n  if (trim(test_mode) /= '1') error stop 2\n  print *, trim(test_mode)\nend program\n",
        "f90",
    );
    let out = unique_path("get_environment_variable_status_keyword_gap", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("status-keyword get_environment_variable compile spawn failed");
    assert!(
        compile.status.success(),
        "status-keyword get_environment_variable should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .env("FORTSH_TEST_MODE", "1")
        .output()
        .expect("run failed");
    assert!(
        run.status.success(),
        "status-keyword get_environment_variable should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains('1'),
        "unexpected status-keyword get_environment_variable output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn get_environment_variable_status_only_keyword_uses_correct_slot() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: stat_out\n  stat_out = -1\n  call get_environment_variable('FORTSH_TEST_MODE', status=stat_out)\n  if (stat_out /= 0) error stop 1\n  print *, stat_out\nend program\n",
        "f90",
    );
    let out = unique_path("get_environment_variable_status_only_keyword", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("status-only get_environment_variable compile spawn failed");
    assert!(
        compile.status.success(),
        "status-only get_environment_variable should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .env("FORTSH_TEST_MODE", "1")
        .output()
        .expect("run failed");
    assert!(
        run.status.success(),
        "status-only get_environment_variable should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains('0'),
        "unexpected status-only get_environment_variable output: {}",
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
fn fixed_char_component_array_actual_preserves_bounds_in_assumed_shape_dummy() {
    let src = write_program(
        "program p\n  implicit none\n  integer, parameter :: max_path_len = 32\n  type :: opts_t\n    character(len=max_path_len) :: globs(4) = ''\n    integer :: n = 0\n  end type opts_t\n  type(opts_t) :: opts\n  opts%globs(1) = 'foo'\n  opts%n = 1\n  call check(opts%globs, opts%n)\ncontains\n  subroutine check(globs, n)\n    character(len=max_path_len), intent(in) :: globs(:)\n    integer, intent(in) :: n\n    if (lbound(globs, 1) /= 1) error stop 1\n    if (ubound(globs, 1) /= 4) error stop 2\n    if (n > 0 .and. trim(globs(1)) /= 'foo') error stop 3\n    print *, trim(globs(1))\n  end subroutine check\nend program\n",
        "f90",
    );
    let out = unique_path("fixed_component_char_array_actual_bounds", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("fixed component char array actual compile failed to spawn");
    assert!(
        compile.status.success(),
        "fixed component char array actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("fixed component char array actual run failed");
    assert!(
        run.status.success(),
        "fixed component char array actual run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("foo"),
        "unexpected fixed component char array actual output: {}",
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
fn derived_function_result_keeps_unallocated_allocatable_char_components_unallocated() {
    let src = write_program(
        "module m\n  implicit none\n  type :: options_t\n    logical :: cleanup_on_close = .true.\n    character(len=:), allocatable :: prefix\n    character(len=:), allocatable :: suffix\n    character(len=:), allocatable :: parent_dir\n  end type options_t\ncontains\n  function clear_options() result(options)\n    type(options_t) :: options\n    options%cleanup_on_close = .true.\n  end function\nend module\n\nprogram p\n  use m\n  implicit none\n  type(options_t) :: options\n  options = clear_options()\n  if (.not. options%cleanup_on_close) error stop 1\n  if (allocated(options%prefix)) error stop 2\n  if (allocated(options%suffix)) error stop 3\n  if (allocated(options%parent_dir)) error stop 4\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("derived_result_unalloc_char_components", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived result unalloc char component compile failed to spawn");
    assert!(
        compile.status.success(),
        "derived result unalloc char component compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("derived result unalloc char component run failed");
    assert!(
        run.status.success(),
        "derived result unalloc char component run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected derived result unalloc char component output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn derived_array_growth_keeps_unallocated_allocatable_components_clear() {
    let src = write_program(
        "program p\n  implicit none\n  type :: trans_t\n    integer :: target = 0\n  end type\n  type :: state_t\n    type(trans_t), allocatable :: trans(:)\n    integer :: num_trans = 0\n  end type\n  type :: nfa_t\n    type(state_t), allocatable :: states(:)\n    integer :: num_states = 0\n  end type\n  type(nfa_t) :: nfa\n  type(trans_t) :: trans\n  integer :: i, start_state, accept_state, prev_accept, s2\n\n  call init(nfa)\n  start_state = add_state(nfa)\n  accept_state = add_state(nfa)\n  trans%target = accept_state\n  call add_trans(nfa%states(start_state), trans)\n\n  do i = 2, 8\n    prev_accept = accept_state\n    s2 = add_state(nfa)\n    trans%target = s2\n    call add_trans(nfa%states(prev_accept), trans)\n    accept_state = s2\n  end do\n\n  if (nfa%states(8)%num_trans /= 1) error stop 1\n  if (.not. allocated(nfa%states(8)%trans)) error stop 2\n  if (nfa%states(8)%trans(1)%target /= 9) error stop 3\n  print *, 'ok'\ncontains\n  subroutine add_trans(state, trans)\n    type(state_t), intent(inout) :: state\n    type(trans_t), intent(in) :: trans\n    type(trans_t), allocatable :: temp(:)\n    integer :: n\n    if (.not. allocated(state%trans)) then\n      allocate(state%trans(4))\n      state%num_trans = 0\n    end if\n    n = state%num_trans\n    if (n >= size(state%trans)) then\n      allocate(temp(size(state%trans) * 2))\n      temp(1:n) = state%trans(1:n)\n      call move_alloc(temp, state%trans)\n    end if\n    state%num_trans = n + 1\n    state%trans(state%num_trans) = trans\n  end subroutine\n\n  subroutine init(nfa)\n    type(nfa_t), intent(inout) :: nfa\n    allocate(nfa%states(8))\n    nfa%num_states = 0\n  end subroutine\n\n  integer function add_state(nfa) result(idx)\n    type(nfa_t), intent(inout) :: nfa\n    type(state_t), allocatable :: temp(:)\n    integer :: n\n    n = nfa%num_states\n    if (n >= size(nfa%states)) then\n      allocate(temp(size(nfa%states) * 2))\n      temp(1:n) = nfa%states(1:n)\n      call move_alloc(temp, nfa%states)\n    end if\n    nfa%num_states = n + 1\n    idx = nfa%num_states\n  end function\nend program\n",
        "f90",
    );
    let out = unique_path("derived_array_growth_unalloc_components", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived array growth compile failed to spawn");
    assert!(
        compile.status.success(),
        "derived array growth compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("derived array growth run failed");
    assert!(
        run.status.success(),
        "derived array growth run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected derived array growth output: {}",
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
fn implicit_len1_char_component_assignment_preserves_bytes() {
    let src = write_program(
        "program p\n  implicit none\n  type :: lexer_state_t\n    character(len=:), allocatable :: input\n    character :: current_char\n  end type\n  type(lexer_state_t) :: lexer\n  lexer%current_char = '2'\n  if (iachar(lexer%current_char) /= iachar('2')) error stop 1\n  lexer%input = trim(adjustl('2 + 3'))\n  lexer%current_char = lexer%input(1:1)\n  if (iachar(lexer%current_char) /= iachar('2')) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("implicit_len1_char_component_assignment", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-O2", "-o", out.to_str().unwrap()])
        .output()
        .expect("implicit len1 char component compile failed to spawn");
    assert!(
        compile.status.success(),
        "implicit len1 char component compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("implicit len1 char component run failed");
    assert!(
        run.status.success(),
        "implicit len1 char component run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("ok"), "unexpected stdout: {}", stdout);

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
fn logical_component_array_copy_preserves_elements() {
    let src = write_program(
        "program p\n  implicit none\n  type :: token_t\n    logical :: bits(0:255)\n  end type\n  type :: node_t\n    logical :: bits(0:255)\n  end type\n  type(token_t) :: tok\n  type(node_t) :: node\n  tok%bits = .false.\n  tok%bits(iachar('a')) = .true.\n  node%bits = tok%bits\n  if (.not. node%bits(iachar('a'))) error stop 1\n  if (node%bits(iachar('z'))) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("logical_component_array_copy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("logical component array copy compile failed to spawn");
    assert!(
        compile.status.success(),
        "logical component array copy compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("logical component array copy run failed");
    assert!(
        run.status.success(),
        "logical component array copy run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected logical component array copy output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn component_array_section_bound_expr_preserves_derived_elements() {
    let src = write_program(
        "program p\n  implicit none\n  type :: token_t\n    integer :: t = 0\n    integer :: pos = 0\n    logical :: bits(0:255) = .false.\n  end type\n  type :: list_t\n    type(token_t), allocatable :: tokens(:)\n    integer :: count = 0\n    integer :: capacity = 0\n  end type\n  type(list_t) :: list\n  type(token_t), allocatable :: temp(:)\n  type(token_t) :: tok\n  integer :: i, idx\n  list%capacity = 32\n  allocate(list%tokens(list%capacity))\n  do i = 1, 32\n    tok = token_t()\n    tok%t = i\n    tok%pos = i\n    idx = mod(i, 256)\n    tok%bits(idx) = .true.\n    list%count = list%count + 1\n    list%tokens(list%count) = tok\n  end do\n  allocate(temp(list%count))\n  temp = list%tokens(1:list%count)\n  do i = 1, 32\n    idx = mod(i, 256)\n    if (temp(i)%t /= i) error stop 100 + i\n    if (temp(i)%pos /= i) error stop 200 + i\n    if (.not. temp(i)%bits(idx)) error stop 300 + i\n  end do\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("component_array_section_bound_expr", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("component array section bound expr compile failed to spawn");
    assert!(
        compile.status.success(),
        "component array section bound expr compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("component array section bound expr run failed");
    assert!(
        run.status.success(),
        "component array section bound expr run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected component array section bound expr output: {}",
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

#[test]
fn local_logical_array_broadcast_does_not_clobber_prior_stack_local() {
    let src = write_program(
        "program p\n  implicit none\n  type :: payload_t\n    character(len=16) :: text = ''\n  end type\n  call wrapper()\n  print *, 'ok'\ncontains\n  subroutine fill(payload, flags, line)\n    type(payload_t), intent(in) :: payload\n    logical, intent(out) :: flags(256)\n    character(len=:), allocatable, intent(out) :: line\n    flags = .false.\n    line = payload%text\n  end subroutine\n\n  subroutine wrapper()\n    type(payload_t) :: payload\n    logical :: flags(256)\n    character(len=:), allocatable :: line\n    payload%text = 'hello'\n    call fill(payload, flags, line)\n    if (trim(line) /= 'hello') error stop 1\n  end subroutine\nend program\n",
        "f90",
    );
    let out = unique_path("logical_array_stack_overlap", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("logical array stack overlap compile failed to spawn");
    assert!(
        compile.status.success(),
        "logical array stack overlap compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("logical array stack overlap run failed");
    assert!(
        run.status.success(),
        "logical array stack overlap run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected logical array stack overlap output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_dummy_realloc_section_assign_returns_with_live_descriptor() {
    let src = write_program(
        "program p\n  implicit none\n  integer, allocatable :: vals(:)\n  allocate(vals(16))\n  vals(1) = 2\n  call resize(vals)\n  if (.not. allocated(vals)) error stop 1\n  if (size(vals) /= 1) error stop 2\n  if (vals(1) /= 7) error stop 3\n  print *, 'ok'\ncontains\n  subroutine resize(vals)\n    integer, allocatable, intent(inout) :: vals(:)\n    integer, allocatable :: temp(:)\n    allocate(temp(1))\n    temp(1) = 7\n    deallocate(vals)\n    allocate(vals(1))\n    vals = temp(1:1)\n    deallocate(temp)\n  end subroutine\nend program\n",
        "f90",
    );
    let out = unique_path("alloc_dummy_section_assign", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable dummy section assign compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocatable dummy section assign compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("allocatable dummy section assign run failed");
    assert!(
        run.status.success(),
        "allocatable dummy section assign run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected allocatable dummy section assign output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_integer_unary_section_assignment_reallocates_to_slice_shape() {
    let src = write_program(
        "program p\n  implicit none\n  integer, allocatable :: vals(:)\n  integer :: ii\n  allocate(vals(4))\n  vals = [1, 3, 5, 7]\n  ii = 3\n  vals = -vals(:ii)\n  if (size(vals) /= 3) error stop 1\n  if (vals(1) /= -1) error stop 2\n  if (vals(2) /= -3) error stop 3\n  if (vals(3) /= -5) error stop 4\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("alloc_int_unary_section_assign", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable integer unary section assign compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocatable integer unary section assign compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("allocatable integer unary section assign run failed");
    assert!(
        run.status.success(),
        "allocatable integer unary section assign run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected allocatable integer unary section assign output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn allocatable_logical_not_section_assignment_reallocates_to_slice_shape() {
    let src = write_program(
        "program p\n  implicit none\n  logical, allocatable :: vals(:)\n  integer :: ii\n  allocate(vals(4))\n  vals = [.true., .false., .true., .false.]\n  ii = 3\n  vals = .not. vals(:ii)\n  if (size(vals) /= 3) error stop 1\n  if (vals(1)) error stop 2\n  if (.not. vals(2)) error stop 3\n  if (vals(3)) error stop 4\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("alloc_logical_not_section_assign", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("allocatable logical not section assign compile failed to spawn");
    assert!(
        compile.status.success(),
        "allocatable logical not section assign compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("allocatable logical not section assign run failed");
    assert!(
        run.status.success(),
        "allocatable logical not section assign run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected allocatable logical not section assign output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn contained_subroutine_shadows_unrelated_global_generic_of_same_name() {
    let src = write_program(
        "module m\n  implicit none\n  type :: token_t\n    integer :: kind = 0\n  end type\n  interface resize\n    module procedure :: resize_token\n  end interface\ncontains\n  pure subroutine resize_token(var, n)\n    type(token_t), allocatable, intent(inout) :: var(:)\n    integer, intent(in), optional :: n\n    integer :: new_size\n    new_size = 1\n    if (present(n)) new_size = n\n    if (.not.allocated(var)) allocate(var(new_size))\n  end subroutine\nend module\nprogram p\n  use m, only : token_t\n  implicit none\n  integer, allocatable :: vals(:)\n  call outer(vals)\n  if (.not.allocated(vals)) error stop 1\n  if (size(vals) /= 2) error stop 2\n  if (any(vals /= 7)) error stop 3\n  print *, 'ok'\ncontains\n  subroutine outer(stack)\n    integer, allocatable, intent(out) :: stack(:)\n    allocate(stack(1))\n    stack = 1\n    call resize(stack)\n  contains\n    subroutine resize(stack, n)\n      integer, allocatable, intent(inout) :: stack(:)\n      integer, intent(in), optional :: n\n      integer, allocatable :: tmp(:)\n      integer :: new_size\n      new_size = 2\n      if (present(n)) new_size = n\n      allocate(tmp(new_size))\n      tmp = 7\n      call move_alloc(tmp, stack)\n    end subroutine\n  end subroutine\nend program\n",
        "f90",
    );
    let out = unique_path("contained_resize_shadows_generic", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("contained resize shadow compile failed to spawn");
    assert!(
        compile.status.success(),
        "contained resize shadow compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("contained resize shadow run failed");
    assert!(
        run.status.success(),
        "contained resize shadow run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected contained resize shadow output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn defined_assignment_generic_does_not_capture_intrinsic_assignment_of_other_derived_arrays() {
    let src = write_program(
        "module m\n  implicit none\n  type :: value_t\n    integer :: x = 0\n  end type\n  type :: token_t\n    integer :: token_type = 0\n    character(len=:), allocatable :: text\n    integer :: position = 0\n  end type\n  public :: assignment(=)\n  interface assignment(=)\n    module procedure assign_value\n  end interface\ncontains\n  subroutine assign_value(lhs, rhs)\n    type(value_t), intent(out) :: lhs\n    type(value_t), intent(in) :: rhs\n    lhs%x = rhs%x\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(token_t), allocatable :: a(:), b(:)\n  allocate(a(2), b(1))\n  a(1)%token_type = 1\n  a(1)%text = '2'\n  a(1)%position = 1\n  b = a(1:1)\n  if (b(1)%token_type /= 1) error stop 1\n  if (trim(b(1)%text) /= '2') error stop 2\n  if (b(1)%position /= 1) error stop 3\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("defined_assignment_intrinsic_token", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("defined assignment intrinsic token compile failed to spawn");
    assert!(
        compile.status.success(),
        "defined assignment intrinsic token compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("defined assignment intrinsic token run failed");
    assert!(
        run.status.success(),
        "defined assignment intrinsic token run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected defined assignment intrinsic token output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn unrelated_defined_assignment_does_not_double_evaluate_intrinsic_scalar_assignment_rhs() {
    let src = write_program(
        "module m\n  implicit none\n  private\n  public :: token_t, lexer_state_t, tokenize, assignment(=), TOKEN_NUMBER, TOKEN_OPERATOR\n  integer, parameter :: TOKEN_EOF = 0, TOKEN_NUMBER = 1, TOKEN_OPERATOR = 3\n  type :: value_t\n    integer :: x = 0\n  end type\n  type :: token_t\n    integer :: token_type = TOKEN_EOF\n    character(len=:), allocatable :: text\n    integer :: position = 0\n  end type\n  type :: lexer_state_t\n    character(len=:), allocatable :: input\n    integer :: position\n    integer :: length\n    character :: current_char\n  end type\n  interface assignment(=)\n    module procedure assign_value\n  end interface\ncontains\n  subroutine assign_value(lhs, rhs)\n    type(value_t), intent(out) :: lhs\n    type(value_t), intent(in) :: rhs\n    lhs%x = rhs%x\n  end subroutine\n\n  function tokenize(expression) result(tokens)\n    character(len=*), intent(in) :: expression\n    type(token_t), allocatable :: tokens(:)\n    type(lexer_state_t) :: lexer\n    type(token_t) :: current_token\n    integer :: token_count\n    lexer%input = trim(adjustl(expression))\n    lexer%length = len_trim(lexer%input)\n    lexer%position = 1\n    lexer%current_char = lexer%input(1:1)\n    allocate(tokens(4))\n    token_count = 0\n    do\n      call skip_whitespace(lexer)\n      if (lexer%current_char == char(0) .or. lexer%position > lexer%length) exit\n      current_token = next_token(lexer)\n      token_count = token_count + 1\n      tokens(token_count) = current_token\n      if (current_token%token_type == TOKEN_EOF) exit\n    end do\n    token_count = token_count + 1\n    tokens(token_count)%token_type = TOKEN_EOF\n  end function\n\n  function next_token(lexer) result(token)\n    type(lexer_state_t), intent(inout) :: lexer\n    type(token_t) :: token\n    token%position = lexer%position\n    select case (lexer%current_char)\n    case ('0':'9')\n      token = read_number(lexer)\n    case ('+')\n      token%text = '+'\n      call advance(lexer)\n      token%token_type = TOKEN_OPERATOR\n    case default\n      token%text = lexer%current_char\n      call advance(lexer)\n      token%token_type = TOKEN_OPERATOR\n    end select\n  end function\n\n  function read_number(lexer) result(token)\n    type(lexer_state_t), intent(inout) :: lexer\n    type(token_t) :: token\n    integer :: start_pos\n    start_pos = lexer%position\n    do while (lexer%current_char >= '0' .and. lexer%current_char <= '9')\n      call advance(lexer)\n    end do\n    token%token_type = TOKEN_NUMBER\n    token%text = lexer%input(start_pos:lexer%position-1)\n  end function\n\n  subroutine skip_whitespace(lexer)\n    type(lexer_state_t), intent(inout) :: lexer\n    do while (lexer%current_char == ' ' .and. lexer%position <= lexer%length)\n      call advance(lexer)\n    end do\n  end subroutine\n\n  subroutine advance(lexer)\n    type(lexer_state_t), intent(inout) :: lexer\n    lexer%position = lexer%position + 1\n    if (lexer%position <= lexer%length) then\n      lexer%current_char = lexer%input(lexer%position:lexer%position)\n    else\n      lexer%current_char = char(0)\n    end if\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(token_t), allocatable :: tokens(:)\n  tokens = tokenize('2 + 3')\n  if (tokens(1)%token_type /= TOKEN_NUMBER) error stop 1\n  if (trim(tokens(1)%text) /= '2') error stop 2\n  if (tokens(2)%token_type /= TOKEN_OPERATOR) error stop 3\n  if (trim(tokens(2)%text) /= '+') error stop 4\n  if (tokens(3)%token_type /= TOKEN_NUMBER) error stop 5\n  if (trim(tokens(3)%text) /= '3') error stop 6\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("defined_assignment_scalar_rhs_once", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-O2", "-o", out.to_str().unwrap()])
        .output()
        .expect("defined assignment scalar rhs-once compile failed to spawn");
    assert!(
        compile.status.success(),
        "defined assignment scalar rhs-once compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("defined assignment scalar rhs-once run failed");
    assert!(
        run.status.success(),
        "defined assignment scalar rhs-once run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected defined assignment scalar rhs-once output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn direct_allocatable_array_function_actual_passes_live_descriptor() {
    let dir = unique_dir("direct_alloc_array_actual");
    let mod_src = write_program_in(
        &dir,
        "producer.f90",
        "module producer\ncontains\n  function make_vals() result(vals)\n    implicit none\n    integer, allocatable :: vals(:)\n    allocate(vals(2))\n    vals = [2, 3]\n  end function\nend module\n",
    );
    let user_src = write_program_in(
        &dir,
        "user.f90",
        "program p\n  use producer, only: make_vals\n  implicit none\n  call show(make_vals())\n  print *, 'ok'\ncontains\n  subroutine show(vals)\n    integer, intent(in) :: vals(:)\n    if (size(vals) /= 2) error stop 1\n    if (lbound(vals, 1) /= 1) error stop 2\n    if (ubound(vals, 1) /= 2) error stop 3\n    if (vals(1) /= 2 .or. vals(2) /= 3) error stop 4\n  end subroutine\nend program\n",
    );

    let mod_obj = dir.join("producer.o");
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
        .expect("producer compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "producer compile failed: {}",
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
        "user compile failed: {}",
        String::from_utf8_lossy(&compile_user.stderr)
    );

    let exe = dir.join("user.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            user_obj.to_str().unwrap(),
            mod_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("direct allocatable array actual run failed");
    assert!(
        run.status.success(),
        "direct allocatable array actual run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected direct allocatable array actual output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn contained_subroutine_actual_to_procedure_dummy_links_and_runs() {
    let src = write_program(
        "module callbacks\n  implicit none\n  abstract interface\n    subroutine cb(value)\n      integer, intent(in) :: value\n    end subroutine\n  end interface\ncontains\n  subroutine remember(handler)\n    procedure(cb) :: handler\n  end subroutine\nend module\nprogram p\n  use callbacks\n  implicit none\n  call remember(local_handler)\n  print *, 'ok'\ncontains\n  subroutine local_handler(value)\n    integer, intent(in) :: value\n    if (value < 0) error stop 1\n  end subroutine\nend program\n",
        "f90",
    );
    let out = unique_path("contained_proc_actual_dummy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("contained proc actual compile failed to spawn");
    assert!(
        compile.status.success(),
        "contained proc actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("contained proc actual run failed");
    assert!(
        run.status.success(),
        "contained proc actual run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected contained proc actual output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn contained_subroutine_pointer_component_assignment_links_and_runs() {
    let src = write_program(
        "module callbacks\n  implicit none\n  abstract interface\n    subroutine cb(value)\n      integer, intent(in) :: value\n    end subroutine\n  end interface\n  type :: holder_t\n    procedure(cb), pointer, nopass :: handler => null()\n  end type\ncontains\n  subroutine invoke(holder)\n    type(holder_t), intent(in) :: holder\n    call holder%handler(7)\n  end subroutine\nend module\nprogram p\n  use callbacks\n  implicit none\n  type(holder_t) :: holder\n  holder%handler => local_handler\n  call invoke(holder)\n  print *, 'ok'\ncontains\n  subroutine local_handler(value)\n    integer, intent(in) :: value\n    if (value /= 7) error stop 1\n  end subroutine\nend program\n",
        "f90",
    );
    let out = unique_path("contained_proc_component_assignment", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("contained proc component assignment compile failed to spawn");
    assert!(
        compile.status.success(),
        "contained proc component assignment should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("contained proc component assignment run failed");
    assert!(
        run.status.success(),
        "contained proc component assignment should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected contained proc component assignment output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn o2_keeps_address_taken_contained_callback_symbols() {
    let src = write_program(
        "module callbacks\n  implicit none\n  abstract interface\n    subroutine cb(value)\n      integer, intent(in) :: value\n    end subroutine\n  end interface\ncontains\n  subroutine remember(handler)\n    procedure(cb) :: handler\n  end subroutine\nend module\nprogram p\n  use callbacks\n  implicit none\n  call remember(local_handler)\n  print *, 'ok'\ncontains\n  subroutine local_handler(value)\n    integer, intent(in) :: value\n    if (value < 0) error stop 1\n  end subroutine\nend program\n",
        "f90",
    );
    let out = unique_path("contained_proc_actual_dummy_o2", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args(["-O2", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("contained proc actual O2 compile failed to spawn");
    assert!(
        compile.status.success(),
        "contained proc actual at -O2 should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("contained proc actual O2 run failed");
    assert!(
        run.status.success(),
        "contained proc actual at -O2 should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected contained proc actual O2 output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn contained_program_char_function_inside_adjustl_and_trim_links_and_runs() {
    let src = write_program(
        "program p\n  implicit none\n  call sleep_ms(25)\ncontains\n  subroutine sleep_ms(ms)\n    integer, intent(in) :: ms\n    real :: seconds\n\n    seconds = real(ms) / 1000.0\n    print '(A)', trim(adjustl(real_to_str(seconds)))\n  end subroutine sleep_ms\n\n  function real_to_str(r) result(str)\n    real, intent(in) :: r\n    character(len=32) :: str\n\n    write(str, '(F0.3)') r\n  end function real_to_str\nend program p\n",
        "f90",
    );
    let out = unique_path("contained_program_char_adjustl_trim", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args(["-O2", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("contained program char helper compile failed to spawn");
    assert!(
        compile.status.success(),
        "contained program char helper compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("contained program char helper run failed");
    assert!(
        run.status.success(),
        "contained program char helper run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("0.025"),
        "unexpected contained program char helper output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn contained_program_char_function_inside_execute_command_line_arg_links_and_runs() {
    let src = write_program(
        "program p\n  implicit none\n  call sleep_ms(25)\ncontains\n  subroutine sleep_ms(ms)\n    integer, intent(in) :: ms\n    real :: seconds\n\n    seconds = real(ms) / 1000.0\n    call execute_command_line('sleep ' // trim(adjustl(real_to_str(seconds))))\n  end subroutine sleep_ms\n\n  function real_to_str(r) result(str)\n    real, intent(in) :: r\n    character(len=32) :: str\n\n    write(str, '(F0.3)') r\n  end function real_to_str\nend program p\n",
        "f90",
    );
    let out = unique_path("contained_program_char_exec_cmd", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args(["-O2", src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("contained execute_command_line compile failed to spawn");
    assert!(
        compile.status.success(),
        "contained execute_command_line compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("contained execute_command_line run failed");
    assert!(
        run.status.success(),
        "contained execute_command_line run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn associate_alias_preserves_component_allocated_and_size_intrinsics() {
    let src = write_program(
        "module m\n  implicit none\n  type :: cursor_t\n    integer :: line = 0\n  end type\n  type :: pane_t\n    type(cursor_t), allocatable :: cursors(:)\n    character(len=:), allocatable :: filename\n  end type\ncontains\n  subroutine check(panes)\n    type(pane_t), intent(inout) :: panes(:)\n    associate (pane => panes(1))\n      if (.not. allocated(pane%cursors)) error stop 1\n      if (size(pane%cursors) /= 2) error stop 2\n      if (.not. allocated(pane%filename)) error stop 3\n      if (trim(pane%filename) /= 'abc') error stop 4\n    end associate\n    print *, 'ok'\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(pane_t), allocatable :: panes(:)\n  allocate(panes(1))\n  allocate(panes(1)%cursors(2))\n  allocate(character(len=3) :: panes(1)%filename)\n  panes(1)%filename = 'abc'\n  call check(panes)\nend program\n",
        "f90",
    );
    let out = unique_path("associate_alias_component_intrinsics", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("associate alias compile failed to spawn");
    assert!(
        compile.status.success(),
        "associate alias compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("associate alias run failed");
    assert!(
        run.status.success(),
        "associate alias run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected associate alias output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn deferred_char_array_component_size_with_dimension_attr_runs() {
    let src = write_program(
        "module m\n  implicit none\n  type :: item_t\n    character(len=:), allocatable, dimension(:) :: lines\n  end type\ncontains\n  subroutine show(total)\n    integer, intent(out) :: total\n    type(item_t) :: one\n    type(item_t) :: many(1)\n    allocate(character(len=8) :: one%lines(2))\n    allocate(character(len=8) :: many(1)%lines(3))\n    one%lines(1) = 'a'\n    one%lines(2) = 'b'\n    many(1)%lines(1) = 'x'\n    many(1)%lines(2) = 'y'\n    many(1)%lines(3) = 'z'\n    total = size(one%lines) + size(many(1)%lines)\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  integer :: total\n  call show(total)\n  if (total /= 5) error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("deferred_char_component_size_dimension_attr", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("deferred char array component size compile failed to spawn");
    assert!(
        compile.status.success(),
        "deferred char array component size compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("deferred char array component size run failed");
    assert!(
        run.status.success(),
        "deferred char array component size run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected deferred char array component size output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn logical_reduction_intrinsics_scalarize_component_and_constructor_exprs() {
    let src = write_program(
        "program p\n  implicit none\n  type :: label_t\n    integer :: first\n  end type\n  type :: line_token\n    integer :: first\n    integer :: last\n  end type\n  type(line_token) :: token(2)\n  type(label_t) :: label\n  integer :: line(2)\n  integer :: total\n  character(len=2) :: input\n\n  token(1)%first = 1\n  token(1)%last = 1\n  token(2)%first = 2\n  token(2)%last = 2\n  label%first = 2\n  line = [1, 2]\n  input = 'aa'\n\n  total = count(token%first < label%first)\n  total = total + count(token%first <= label%first)\n  if (all([input(1:1), input(2:2)] == 'a')) total = total + 1\n  if (any(1 == line)) total = total + 1\n\n  if (total /= 5) error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("logical_reduce_scalarized_exprs", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("logical reduction scalarized compile failed to spawn");
    assert!(
        compile.status.success(),
        "logical reduction scalarized compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("logical reduction scalarized run failed");
    assert!(
        run.status.success(),
        "logical reduction scalarized run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected logical reduction scalarized output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn logical_reduction_intrinsics_on_logical_sections_compile_and_run() {
    let src = write_program(
        "program p\n  implicit none\n  logical :: mask(4)\n  integer :: total\n\n  mask = [.true., .false., .true., .true.]\n  total = count(mask(2:4))\n  if (any(mask(2:3))) total = total + 10\n  if (all(mask(3:4))) total = total + 100\n\n  if (total /= 112) error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("logical_reduce_sections", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("logical reduction section compile failed to spawn");
    assert!(
        compile.status.success(),
        "logical reduction section compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("logical reduction section run failed");
    assert!(
        run.status.success(),
        "logical reduction section run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected logical reduction section output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn logical_reduction_intrinsics_on_elemental_character_results_compile_and_run() {
    let src = write_program(
        "program p\n  implicit none\n  integer :: pos(2)\n  character(len=1) :: kind(2)\n\n  pos = [1, 2]\n  kind = ['a', 'b']\n\n  if (.not. all(peek('ab', pos) == kind)) error stop 1\n  if (.not. any(match('ab', pos, ['x', 'b']))) error stop 2\n\n  print *, 'ok'\ncontains\n  elemental function peek(src, idx) result(ch)\n    character(len=*), intent(in) :: src\n    integer, intent(in) :: idx\n    character(len=1) :: ch\n    ch = src(idx:idx)\n  end function\n\n  elemental logical function match(src, idx, want) result(ok)\n    character(len=*), intent(in) :: src\n    integer, intent(in) :: idx\n    character(len=1), intent(in) :: want\n    ok = src(idx:idx) == want\n  end function\nend program\n",
        "f90",
    );
    let out = unique_path("logical_reduce_elemental_char_results", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("logical reduction elemental char compile failed to spawn");
    assert!(
        compile.status.success(),
        "logical reduction elemental char compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("logical reduction elemental char run failed");
    assert!(
        run.status.success(),
        "logical reduction elemental char run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected logical reduction elemental char output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn logical_reduction_intrinsics_on_component_char_constructor_actuals_compile_and_run() {
    let src = write_program(
        "program p\n  implicit none\n  type :: chars_t\n    character(len=1) :: space\n    character(len=1) :: tab\n  end type\n  type(chars_t), parameter :: char_kind = chars_t(' ', achar(9))\n\n  if (.not. any(match(' \t', 1, [char_kind%space, char_kind%tab]))) error stop 1\n\n  print *, 'ok'\ncontains\n  elemental logical function match(src, idx, want) result(ok)\n    character(len=*), intent(in) :: src\n    integer, intent(in) :: idx\n    character(len=1), intent(in) :: want\n    ok = src(idx:idx) == want\n  end function\nend program\n",
        "f90",
    );
    let out = unique_path("logical_reduce_component_ctor_actuals", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("logical reduction component ctor compile failed to spawn");
    assert!(
        compile.status.success(),
        "logical reduction component ctor compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("logical reduction component ctor run failed");
    assert!(
        run.status.success(),
        "logical reduction component ctor run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected logical reduction component ctor output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn logical_reduction_intrinsics_on_nonmatching_component_char_constructor_actuals_compile_and_run() {
    let src = write_program(
        "program p\n  implicit none\n  type :: chars_t\n    character(len=1) :: equal\n    character(len=1) :: space\n    character(len=1) :: tab\n  end type\n  type(chars_t), parameter :: char_kind = chars_t('=', ' ', achar(9))\n\n  if (any(match(char_kind%equal, 1, [char_kind%space, char_kind%tab]))) error stop 1\n\n  print *, 'ok'\ncontains\n  elemental logical function match(src, idx, want) result(ok)\n    character(len=*), intent(in) :: src\n    integer, intent(in) :: idx\n    character(len=1), intent(in) :: want\n    ok = src(idx:idx) == want\n  end function\nend program\n",
        "f90",
    );
    let out = unique_path("logical_reduce_component_ctor_actuals_nonmatching", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("logical reduction nonmatching component ctor compile failed to spawn");
    assert!(
        compile.status.success(),
        "logical reduction nonmatching component ctor compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("logical reduction nonmatching component ctor run failed");
    assert!(
        run.status.success(),
        "logical reduction nonmatching component ctor run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected logical reduction nonmatching component ctor output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn derived_scalar_structure_constructor_initializer_sets_char_components() {
    let src = write_program(
        "program p\n  implicit none\n  type :: chars_t\n    character(len=1) :: space\n    character(len=1) :: tab\n  end type\n  type(chars_t) :: char_kind = chars_t(' ', achar(9))\n  if (iachar(char_kind%space) /= 32) error stop 1\n  if (iachar(char_kind%tab) /= 9) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("derived_scalar_ctor_initializer_chars", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived scalar ctor initializer compile failed to spawn");
    assert!(
        compile.status.success(),
        "derived scalar ctor initializer compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("derived scalar ctor initializer run failed");
    assert!(
        run.status.success(),
        "derived scalar ctor initializer run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected derived scalar ctor initializer output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn procedure_dummy_with_explicit_interface_indirect_call_links_and_runs() {
    let src = write_program(
        "module m\n  implicit none\n  abstract interface\n    logical function compare_less(lhs, rhs) result(less)\n      integer, intent(in) :: lhs, rhs\n    end function compare_less\n  end interface\ncontains\n  subroutine quickcheck(x, low, high, less)\n    integer, intent(inout) :: x(:)\n    integer, intent(in) :: low, high\n    procedure(compare_less) :: less\n    if (low < high) then\n      if (.not. less(x(low), x(high))) error stop 1\n    end if\n  end subroutine quickcheck\n\n  function compare_int(lhs, rhs) result(less)\n    integer, intent(in) :: lhs, rhs\n    logical :: less\n    less = lhs < rhs\n  end function compare_int\nend module m\nprogram p\n  use m\n  implicit none\n  integer :: x(2)\n  x = [1, 2]\n  call quickcheck(x, 1, 2, compare_int)\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("procedure_dummy_interface_indirect_call", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("procedure dummy interface compile failed to spawn");
    assert!(
        compile.status.success(),
        "procedure dummy interface compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("procedure dummy interface run failed");
    assert!(
        run.status.success(),
        "procedure dummy interface run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected procedure dummy interface output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn new_line_intrinsic_links_and_runs_in_runtime_char_context() {
    let src = write_program(
        "program p\n  implicit none\n  character(len=:), allocatable :: text\n  text = 'a' // new_line('a') // 'b'\n  if (len(text) /= 3) error stop 1\n  if (iachar(text(2:2)) /= 10) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("new_line_intrinsic_runtime_context", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("new_line intrinsic compile failed to spawn");
    assert!(
        compile.status.success(),
        "new_line intrinsic compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("new_line intrinsic run failed");
    assert!(
        run.status.success(),
        "new_line intrinsic run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected new_line intrinsic output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn sum_of_array_section_times_constructor_runs() {
    let src = write_program(
        "program p\n  implicit none\n  integer, allocatable :: msec(:)\n  integer :: out\n  msec = [1, 2, 3, 4, 5, 6, 7]\n  out = sum(msec(1:6) * [100000, 10000, 1000, 100, 10, 1])\n  print *, out\nend program\n",
        "f90",
    );
    let out = unique_path("sum_array_section_times_constructor", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "array-expression SUM repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&out).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "array-expression SUM repro should run:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("123456"),
        "expected weighted sum output, got: {}",
        stdout
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn is_iostat_intrinsics_compile_and_run_under_implicit_none() {
    let src = write_program(
        "program p\n  use iso_fortran_env, only : iostat_end, iostat_eor\n  implicit none\n  logical :: a, b\n  a = is_iostat_end(iostat_end)\n  b = is_iostat_eor(iostat_eor)\n  if (.not. a) error stop 1\n  if (.not. b) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("is_iostat_intrinsics", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "is_iostat intrinsics repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&out).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "is_iostat intrinsics repro should run:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("ok"), "expected ok output, got: {}", stdout);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn generic_resolution_uses_scope_local_kind_aliases() {
    let src = write_program(
        "module m\n  implicit none\n  integer, parameter :: i1 = selected_int_kind(2)\n  integer, parameter :: i4 = selected_int_kind(9)\n  interface to_string\n    module procedure :: integer_i1_to_string\n    module procedure :: integer_i4_to_string\n  end interface\ncontains\n  pure function integer_i1_to_string(val) result(string)\n    integer, parameter :: ik = i1\n    integer(ik), intent(in) :: val\n    character(len=:), allocatable :: string\n    string = 'i1'\n  end function\n  pure function integer_i4_to_string(val) result(string)\n    integer, parameter :: ik = i4\n    integer(ik), intent(in) :: val\n    character(len=:), allocatable :: string\n    string = 'i4'\n  end function\nend module m\nprogram p\n  use m\n  implicit none\n  integer :: it\n  it = 1\n  print *, to_string(it)\nend program\n",
        "f90",
    );
    let out = unique_path("generic_scope_local_kind_alias", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "generic local-kind alias repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&out).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "generic local-kind alias repro should run:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.contains("i4"), "expected i4 output, got: {}", stdout);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn generic_deferred_char_function_inside_concat_uses_hidden_result_abi() {
    let src = write_program(
        "module m\n  implicit none\n  integer, parameter :: i4 = selected_int_kind(9)\n  interface to_string\n    module procedure :: integer_i4_to_string\n  end interface\ncontains\n  pure function integer_i4_to_string(val) result(string)\n    integer(i4), intent(in) :: val\n    character(len=:), allocatable :: string\n    if (val == 1_i4) then\n      string = 'i4'\n    else\n      string = 'other'\n    end if\n  end function\nend module m\nprogram p\n  use m\n  implicit none\n  integer :: it\n  character(len=:), allocatable :: s\n  it = 1\n  s = '[' // to_string(it) // ']'\n  if (s /= '[i4]') error stop 1\n  print *, trim(s)\nend program\n",
        "f90",
    );
    let out = unique_path("generic_concat_hidden_string_result", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("spawn failed");
    assert!(
        compile.status.success(),
        "generic deferred-char concat repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&out).output().expect("failed to run binary");
    assert!(
        run.status.success(),
        "generic deferred-char concat repro should run:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("[i4]"),
        "expected [i4] output, got: {}",
        stdout
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn generic_subroutine_resolution_allows_omitted_trailing_optional_args() {
    let src = write_program(
        "module m\n  implicit none\n  type :: t\n    integer :: x = 0\n  end type\n  interface resize\n    module procedure :: resize_t\n  end interface\ncontains\n  pure subroutine resize_t(var, n)\n    type(t), allocatable, intent(inout) :: var(:)\n    integer, intent(in), optional :: n\n    integer :: new_size\n    new_size = 1\n    if (present(n)) new_size = n\n    if (.not.allocated(var)) then\n      allocate(var(new_size))\n    end if\n    var(1)%x = new_size\n  end subroutine\nend module m\nprogram p\n  use m, only : t, resize\n  implicit none\n  type(t), allocatable :: a(:)\n  call resize(a)\n  if (.not.allocated(a)) error stop 1\n  if (size(a) /= 1) error stop 2\n  if (a(1)%x /= 1) error stop 3\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("generic_optional_trailing_arg", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("generic optional trailing arg compile failed to spawn");
    assert!(
        compile.status.success(),
        "generic optional trailing arg compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("generic optional trailing arg run failed");
    assert!(
        run.status.success(),
        "generic optional trailing arg run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected generic optional trailing arg output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn generic_subroutine_resolution_preserves_skipped_optional_keyword_slots() {
    let src = write_program(
        "module m\n  implicit none\n  type :: t\n    integer :: a = 0\n    logical :: b = .false.\n    integer :: c = 0\n  end type\n  interface foo\n    module procedure :: foo_t\n  end interface\ncontains\n  subroutine foo_t(x, a, b, c)\n    type(t), intent(inout) :: x\n    integer, intent(in) :: a\n    logical, intent(in), optional :: b\n    integer, intent(in), optional :: c\n    x%a = a\n    if (present(b)) then\n      x%b = b\n    else\n      x%b = .false.\n    end if\n    if (present(c)) then\n      x%c = c\n    else\n      x%c = -1\n    end if\n  end subroutine\nend module m\nprogram p\n  use m\n  implicit none\n  type(t) :: x\n  call foo(x, 11, c=77)\n  if (x%a /= 11) error stop 1\n  if (x%b) error stop 2\n  if (x%c /= 77) error stop 3\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("generic_keyword_optional_gap", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("generic keyword optional gap compile failed to spawn");
    assert!(
        compile.status.success(),
        "generic keyword optional gap compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("generic keyword optional gap run failed");
    assert!(
        run.status.success(),
        "generic keyword optional gap run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected generic keyword optional gap output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn generic_subroutine_resolution_rejects_keyword_overwrite_candidates() {
    let src = write_program(
        "module m\n  implicit none\n  type :: t\n    integer :: which = 0\n  end type\n  interface set_value\n    module procedure :: set_elem_value_float_sp\n    module procedure :: set_array_value_int_i4\n  end interface\ncontains\n  subroutine set_elem_value_float_sp(array, pos, val, stat, origin)\n    type(t), intent(inout) :: array\n    integer, intent(in) :: pos\n    real, intent(in) :: val\n    integer, intent(out), optional :: stat\n    integer, intent(out), optional :: origin\n    array%which = pos\n    if (present(stat)) stat = 10\n    if (present(origin)) origin = nint(val)\n  end subroutine\n\n  subroutine set_array_value_int_i4(array, val, stat, origin)\n    type(t), intent(inout) :: array\n    integer, intent(in) :: val(:)\n    integer, intent(out), optional :: stat\n    integer, intent(out), optional :: origin\n    array%which = -99\n    if (present(stat)) stat = 20\n    if (present(origin)) origin = size(val)\n  end subroutine\nend module\nprogram p\n  use m, only : t, set_value\n  implicit none\n  type(t) :: array\n  integer :: stat\n  call set_value(array, 7, 3.5, stat=stat)\n  if (array%which /= 7) error stop 1\n  if (stat /= 10) error stop 2\n  print *, array%which, stat\nend program\n",
        "f90",
    );
    let out = unique_path("generic_keyword_overwrite_candidate", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("generic keyword overwrite candidate compile failed to spawn");
    assert!(
        compile.status.success(),
        "generic keyword overwrite candidate compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("generic keyword overwrite candidate run failed");
    assert!(
        run.status.success(),
        "generic keyword overwrite candidate run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    let fields: Vec<&str> = stdout.split_whitespace().collect();
    assert_eq!(
        fields,
        vec!["7", "10"],
        "unexpected generic keyword overwrite candidate output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn derived_constructor_keywords_preserve_parameter_component_defaults() {
    let src = write_program(
        "module m\n  implicit none\n  type :: merge_policy_t\n    integer :: overwrite = 11\n    integer :: preserve = 22\n    integer :: append = 33\n  end type\n  type(merge_policy_t), parameter :: policy = merge_policy_t()\n  type :: cfg_t\n    integer :: table = policy%append\n    integer :: array = policy%preserve\n    integer :: keyval = policy%preserve\n  end type\n  interface cfg_t\n    module procedure :: new_cfg\n  end interface\ncontains\n  function new_cfg(table, array, keyval) result(cfg)\n    integer, intent(in), optional :: table, array, keyval\n    type(cfg_t) :: cfg\n    if (present(table)) cfg%table = table\n    if (present(array)) cfg%array = array\n    if (present(keyval)) cfg%keyval = keyval\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  type(cfg_t) :: cfg\n  cfg = cfg_t(keyval=policy%overwrite)\n  if (cfg%table /= 33) error stop 1\n  if (cfg%array /= 22) error stop 2\n  if (cfg%keyval /= 11) error stop 3\n  print *, cfg%table, cfg%array, cfg%keyval\nend program\n",
        "f90",
    );
    let out = unique_path("derived_ctor_keyword_defaults", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived constructor keyword/default compile failed to spawn");
    assert!(
        compile.status.success(),
        "derived constructor keyword/default compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("derived constructor keyword/default run failed");
    assert!(
        run.status.success(),
        "derived constructor keyword/default run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    let fields: Vec<&str> = stdout.split_whitespace().collect();
    assert_eq!(
        fields,
        vec!["33", "22", "11"],
        "unexpected derived constructor keyword/default output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_generic_subroutine_resolution_uses_specific_keyword_slots() {
    let dir = unique_dir("generic_keyword_specific_slots");
    let mod_src = write_program_in(
        &dir,
        "m.f90",
        "module m\n  implicit none\n  interface check\n    module procedure :: check_logical\n    module procedure :: check_real\n  end interface\ncontains\n  subroutine check_logical(error, actual, expected, message, more)\n    integer, intent(inout) :: error\n    logical, intent(in) :: actual, expected\n    character(*), intent(in), optional :: message\n    character(*), intent(in), optional :: more\n    if (actual .neqv. expected) error = 1\n  end subroutine\n\n  subroutine check_real(error, actual, expected, message, more, thr, rel)\n    integer, intent(inout) :: error\n    real, intent(in) :: actual, expected\n    character(*), intent(in), optional :: message\n    character(*), intent(in), optional :: more\n    real, intent(in), optional :: thr\n    logical, intent(in), optional :: rel\n    real :: tol\n    tol = 0.0\n    if (present(thr)) tol = thr\n    if (abs(actual - expected) > tol) error = 2\n    if (present(rel)) then\n      if (rel) error = error + 10\n    end if\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use m, only : check\n  implicit none\n  integer :: error\n  real :: expected, actual\n  error = 0\n  expected = 1.0\n  actual = expected + epsilon(expected)\n  call check(error, actual, expected, thr=2.0*epsilon(expected))\n  if (error /= 0) error stop 1\n  print *, 'ok'\nend program\n",
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
        .expect("module compile failed to spawn");
    assert!(
        compile_mod.status.success(),
        "generic provider module should compile: {}",
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
        .expect("consumer compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "consumer compile should resolve generic keyword slots through imported .amod: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let out = dir.join("p");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            mod_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("link failed to spawn");
    assert!(
        link.status.success(),
        "linked binary should build: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_generic_subroutine_resolution_matches_int_intrinsic_kind_actuals() {
    let dir = unique_dir("generic_int_kind_actuals");
    let kinds_src = write_program_in(
        &dir,
        "kinds.f90",
        "module kinds\n  implicit none\n  integer, parameter :: tfi = selected_int_kind(18)\nend module\n",
    );
    let checks_src = write_program_in(
        &dir,
        "checks.f90",
        "module checks\n  implicit none\n  interface check\n    module procedure :: check_i4\n    module procedure :: check_i8\n  end interface\ncontains\n  subroutine check_i4(error, actual, expected)\n    integer, intent(inout) :: error\n    integer(4), intent(in) :: actual, expected\n    if (actual /= expected) error = 1\n  end subroutine\n\n  subroutine check_i8(error, actual, expected)\n    integer, intent(inout) :: error\n    integer(8), intent(in) :: actual, expected\n    if (actual /= expected) error = 2\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use kinds, only : tfi\n  use checks, only : check\n  implicit none\n  integer :: error\n  integer(tfi) :: value\n  error = 0\n  value = int(b\"1001\", tfi)\n  call check(error, value, int(b\"1001\", tfi))\n  if (error /= 0) error stop 1\n  print *, 'ok'\nend program\n",
    );

    let kinds_obj = dir.join("kinds.o");
    let compile_kinds = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            kinds_src.to_str().unwrap(),
            "-o",
            kinds_obj.to_str().unwrap(),
        ])
        .output()
        .expect("kinds compile failed to spawn");
    assert!(
        compile_kinds.status.success(),
        "kinds module should compile: {}",
        String::from_utf8_lossy(&compile_kinds.stderr)
    );

    let checks_obj = dir.join("checks.o");
    let compile_checks = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            checks_src.to_str().unwrap(),
            "-o",
            checks_obj.to_str().unwrap(),
        ])
        .output()
        .expect("checks compile failed to spawn");
    assert!(
        compile_checks.status.success(),
        "generic provider module should compile: {}",
        String::from_utf8_lossy(&compile_checks.stderr)
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
        .expect("consumer compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "consumer compile should match generic specifics for int(..., kind): {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let out = dir.join("p");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            kinds_obj.to_str().unwrap(),
            checks_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("link failed to spawn");
    assert!(
        link.status.success(),
        "linked binary should build: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_generic_subroutine_prefers_character_intrinsic_actual_specific() {
    let dir = unique_dir("generic_character_intrinsic_actual");
    let provider_src = write_program_in(
        &dir,
        "provider.f90",
        "module provider\n  implicit none\n  type :: keyval_t\n    integer :: type_code = 0\n    character(:), allocatable :: str\n  contains\n    procedure :: get_type\n  end type\n  interface set_value\n    module procedure :: set_value_i1\n    module procedure :: set_value_string\n  end interface\ncontains\n  pure function get_type(self) result(code)\n    class(keyval_t), intent(in) :: self\n    integer :: code\n    code = self%type_code\n  end function\n\n  subroutine set_value_i1(self, val, stat)\n    type(keyval_t), intent(inout) :: self\n    integer(1), intent(in) :: val\n    integer, intent(out), optional :: stat\n    self%type_code = 103\n    if (allocated(self%str)) deallocate(self%str)\n    if (present(stat)) stat = int(val)\n  end subroutine\n\n  subroutine set_value_string(self, val, stat)\n    type(keyval_t), intent(inout) :: self\n    character(*), intent(in) :: val\n    integer, intent(out), optional :: stat\n    self%type_code = 101\n    self%str = val\n    if (present(stat)) stat = 0\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use provider, only : keyval_t, set_value\n  implicit none\n  type(keyval_t) :: kv\n  integer :: stat\n  call set_value(kv, repeat('a', 3), stat=stat)\n  if (stat /= 0) error stop 1\n  if (kv%get_type() /= 101) error stop 2\n  if (.not.allocated(kv%str)) error stop 3\n  if (len(kv%str) /= 3) error stop 4\n  if (kv%str /= 'aaa') error stop 5\n  print *, 'ok'\nend program\n",
    );

    let provider_obj = dir.join("provider.o");
    let compile_provider = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            provider_src.to_str().unwrap(),
            "-o",
            provider_obj.to_str().unwrap(),
        ])
        .output()
        .expect("provider compile failed to spawn");
    assert!(
        compile_provider.status.success(),
        "provider module should compile: {}",
        String::from_utf8_lossy(&compile_provider.stderr)
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
        "consumer compile should resolve character intrinsic actuals: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let out = dir.join("p");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            provider_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("link failed to spawn");
    assert!(
        link.status.success(),
        "linked binary should build: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn generic_subroutine_accepts_descriptor_backed_character_dummy_actuals() {
    let src = "module m\n  implicit none\n  integer, parameter :: dp = kind(1.0d0)\n  type :: error_type\n    integer :: code = 0\n  end type\n  interface check\n    module procedure :: check_complex_dp\n    module procedure :: check_complex_exceptional_dp\n    module procedure :: check_string\n    module procedure :: check_int_i1\n  end interface\ncontains\n  subroutine check_complex_dp(error, actual, expected, message, more)\n    type(error_type), allocatable, intent(out) :: error\n    complex(dp), intent(in) :: actual\n    complex(dp), intent(in) :: expected\n    character(len=*), intent(in), optional :: message\n    character(len=*), intent(in), optional :: more\n    call check(error, actual, message, more)\n    if (allocated(error)) error stop 1\n  end subroutine\n\n  subroutine check_complex_exceptional_dp(error, actual, message, more)\n    type(error_type), allocatable, intent(out) :: error\n    complex(dp), intent(in) :: actual\n    character(len=*), intent(in), optional :: message\n    character(len=*), intent(in), optional :: more\n    if (present(message)) then\n      if (message /= 'msg') error stop 2\n    end if\n    if (present(more)) then\n      if (more /= 'more') error stop 3\n    end if\n  end subroutine\n\n  subroutine check_string(error, actual, expected, message, more)\n    type(error_type), allocatable, intent(out) :: error\n    character(len=*), intent(in) :: actual\n    character(len=*), intent(in) :: expected\n    character(len=*), intent(in), optional :: message\n    character(len=*), intent(in), optional :: more\n    error stop 4\n  end subroutine\n\n  subroutine check_int_i1(error, actual, expected, message, more)\n    type(error_type), allocatable, intent(out) :: error\n    integer(1), intent(in) :: actual\n    integer(1), intent(in) :: expected\n    character(len=*), intent(in), optional :: message\n    character(len=*), intent(in), optional :: more\n    error stop 5\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(error_type), allocatable :: error\n  call check_complex_dp(error, (1.0_dp, 2.0_dp), (1.0_dp, 2.0_dp), 'msg', 'more')\n  if (allocated(error)) error stop 6\n  print *, 'ok'\nend program\n";
    let src = write_program(src, "f90");
    let out = unique_path("generic_char_dummy_actuals", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("compile failed to spawn");
    assert!(
        result.status.success(),
        "compile should accept descriptor-backed character dummy actuals: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn generic_function_resolution_uses_local_scope_for_intrinsic_result_kinds() {
    let src = write_program(
        "module m\n  implicit none\n  integer, parameter :: sp = selected_real_kind(6)\n  integer, parameter :: dp = selected_real_kind(15)\n  interface is_nan\n    module procedure :: is_nan_sp\n    module procedure :: is_nan_dp\n  end interface\ncontains\n  subroutine earlier(actual)\n    complex(dp), intent(in) :: actual\n    if (is_nan(real(actual)) .or. is_nan(aimag(actual))) error stop 1\n  end subroutine\n\n  subroutine later(actual)\n    complex(sp), intent(in) :: actual\n    logical :: x\n    x = is_nan(real(actual)) .or. is_nan(aimag(actual))\n    if (x) error stop 2\n  end subroutine\n\n  elemental function is_nan_sp(val) result(is_nan)\n    real(sp), intent(in) :: val\n    logical :: is_nan\n    is_nan = val /= val\n  end function\n\n  elemental function is_nan_dp(val) result(is_nan)\n    real(dp), intent(in) :: val\n    logical :: is_nan\n    is_nan = val /= val\n  end function\nend module\nprogram p\n  use m\n  call later((1.0_sp, 2.0_sp))\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("generic_intrinsic_scope_kinds", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("compile failed to spawn");
    assert!(
        result.status.success(),
        "compile should use local complex kinds for intrinsic generic actuals: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn generic_subroutine_accepts_generic_function_character_actuals() {
    let src = write_program(
        "module m\n  implicit none\n  interface to_string\n    module procedure :: to_string_i4\n    module procedure :: to_string_i8\n  end interface\n  interface accept\n    module procedure :: accept_string\n    module procedure :: accept_i8\n  end interface\ncontains\n  function to_string_i4(val) result(string)\n    integer(4), intent(in) :: val\n    character(len=:), allocatable :: string\n    if (val == 7) then\n      string = 'i4'\n    else\n      string = 'bad'\n    end if\n  end function\n\n  function to_string_i8(val) result(string)\n    integer(8), intent(in) :: val\n    character(len=:), allocatable :: string\n    if (val == 7_8) then\n      string = 'i8'\n    else\n      string = 'bad'\n    end if\n  end function\n\n  subroutine accept_string(val)\n    character(*), intent(in) :: val\n    if (val /= 'i4') error stop 2\n  end subroutine\n\n  subroutine accept_i8(val)\n    integer(8), intent(in) :: val\n    if (val /= 0_8) error stop 3\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  integer :: ii\n  ii = 7\n  call accept(to_string(ii))\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("generic_function_character_actual", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("compile failed to spawn");
    assert!(
        result.status.success(),
        "compile should accept generic function character actuals: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn generic_subroutine_accepts_non_boz_int_kind_actuals() {
    let src = write_program(
        "module m\n  implicit none\n  integer, parameter :: i8 = selected_int_kind(18)\n  interface check\n    module procedure :: check_i4\n    module procedure :: check_i8\n  end interface\ncontains\n  subroutine check_i4(val)\n    integer(4), intent(in) :: val\n    if (val /= 0) error stop 1\n  end subroutine\n\n  subroutine check_i8(val)\n    integer(8), intent(in) :: val\n    if (val /= 7_i8) error stop 2\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  integer :: ii\n  ii = 7\n  call check(int(ii, i8))\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("generic_non_boz_int_kind_actual", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("compile failed to spawn");
    assert!(
        result.status.success(),
        "compile should accept non-BOZ int(..., kind) actuals: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_generic_len_shadows_intrinsic_in_generic_dispatch() {
    let dir = unique_dir("generic_len_shadow_dispatch");
    let types_src = write_program_in(
        &dir,
        "types.f90",
        "module types\n  implicit none\n  type :: array_t\n    integer :: n = 0\n  end type\n  interface len\n    module procedure :: get_len\n  end interface\ncontains\n  pure function get_len(self) result(length)\n    type(array_t), intent(in) :: self\n    integer :: length\n    length = self%n\n  end function\nend module\n",
    );
    let checks_src = write_program_in(
        &dir,
        "checks.f90",
        "module checks\n  implicit none\n  interface check\n    module procedure :: check_i4\n    module procedure :: check_i8\n  end interface\ncontains\n  subroutine check_i4(error, actual, expected)\n    integer, intent(inout) :: error\n    integer(4), intent(in) :: actual, expected\n    if (actual /= expected) error = 1\n  end subroutine\n\n  subroutine check_i8(error, actual, expected)\n    integer, intent(inout) :: error\n    integer(8), intent(in) :: actual, expected\n    error = 2\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use types, only : array_t, len\n  use checks, only : check\n  implicit none\n  type(array_t) :: value\n  integer :: error\n  error = 0\n  value%n = 4\n  call check(error, len(value), 4)\n  if (error /= 0) error stop 1\n  print *, 'ok'\nend program\n",
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
        .expect("types compile failed to spawn");
    assert!(
        compile_types.status.success(),
        "types module should compile: {}",
        String::from_utf8_lossy(&compile_types.stderr)
    );

    let checks_obj = dir.join("checks.o");
    let compile_checks = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            checks_src.to_str().unwrap(),
            "-o",
            checks_obj.to_str().unwrap(),
        ])
        .output()
        .expect("checks compile failed to spawn");
    assert!(
        compile_checks.status.success(),
        "checks module should compile: {}",
        String::from_utf8_lossy(&compile_checks.stderr)
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
        "consumer compile should use imported generic len before intrinsic lowering: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let out = dir.join("p");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            types_obj.to_str().unwrap(),
            checks_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("link failed to spawn");
    assert!(
        link.status.success(),
        "linked binary should build: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_same_name_generic_can_fall_back_to_structure_constructor_with_nested_actuals() {
    let dir = unique_dir("imported_same_name_ctor_nested");
    let types_src = write_program_in(
        &dir,
        "types.f90",
        "module types\n  implicit none\n  type :: date_t\n    integer :: year = -1\n    integer :: month = -1\n    integer :: day = -1\n  end type\n\n  type :: time_t\n    integer :: hour = -1\n    integer :: minute = -1\n  end type\n\n  type :: datetime_t\n    type(date_t) :: date\n    type(time_t) :: time\n  end type\n\n  interface datetime_t\n    module procedure :: new_datetime\n    module procedure :: new_datetime_from_string\n  end interface\n\n  interface time_t\n    module procedure :: new_time\n  end interface\ncontains\n  pure function new_datetime(year, month, day, hour, minute) result(value)\n    integer, intent(in), optional :: year, month, day, hour, minute\n    type(datetime_t) :: value\n    if (present(year)) value%date%year = year\n    if (present(month)) value%date%month = month\n    if (present(day)) value%date%day = day\n    if (present(hour)) value%time%hour = hour\n    if (present(minute)) value%time%minute = minute\n  end function\n\n  pure function new_datetime_from_string(string) result(value)\n    character(*), intent(in) :: string\n    type(datetime_t) :: value\n    value%date%year = len_trim(string)\n  end function\n\n  pure function new_time(hour, minute) result(value)\n    integer, intent(in), optional :: hour, minute\n    type(time_t) :: value\n    if (present(hour)) value%hour = hour\n    if (present(minute)) value%minute = minute\n  end function\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use types, only : datetime_t, date_t, time_t\n  implicit none\n  type(datetime_t) :: value\n  value = datetime_t(date_t(2022, 7, 31), time_t())\n  if (value%date%year /= 2022) error stop 1\n  if (value%date%month /= 7) error stop 2\n  if (value%date%day /= 31) error stop 3\n  if (value%time%hour /= -1) error stop 4\n  print *, 'ok'\nend program\n",
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
        .expect("types compile failed to spawn");
    assert!(
        compile_types.status.success(),
        "types module should compile: {}",
        String::from_utf8_lossy(&compile_types.stderr)
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
        "imported same-name generic should fall back to structure constructor: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let out = dir.join("p");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            types_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("link failed to spawn");
    assert!(
        link.status.success(),
        "linked binary should build: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_same_name_generic_constructor_inside_implied_do_array_constructor_runs() {
    let dir = unique_dir("imported_same_name_ctor_implied_do");
    let types_src = write_program_in(
        &dir,
        "types.f90",
        "module types\n  implicit none\n  type :: date_t\n    integer :: year = -1\n    integer :: month = -1\n    integer :: day = -1\n  end type\n\n  type :: time_t\n    integer :: hour = -1\n    integer :: minute = -1\n  end type\n\n  type :: datetime_t\n    type(date_t) :: date\n    type(time_t) :: time\n  end type\n\n  interface datetime_t\n    module procedure :: new_datetime\n    module procedure :: new_datetime_from_string\n  end interface\n\n  interface time_t\n    module procedure :: new_time\n  end interface\ncontains\n  pure function new_datetime(year, month, day, hour, minute) result(value)\n    integer, intent(in), optional :: year, month, day, hour, minute\n    type(datetime_t) :: value\n    if (present(year)) value%date%year = year\n    if (present(month)) value%date%month = month\n    if (present(day)) value%date%day = day\n    if (present(hour)) value%time%hour = hour\n    if (present(minute)) value%time%minute = minute\n  end function\n\n  pure function new_datetime_from_string(string) result(value)\n    character(*), intent(in) :: string\n    type(datetime_t) :: value\n    value%date%year = len_trim(string)\n  end function\n\n  pure function new_time(hour, minute) result(value)\n    integer, intent(in), optional :: hour, minute\n    type(time_t) :: value\n    if (present(hour)) value%hour = hour\n    if (present(minute)) value%minute = minute\n  end function\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use types, only : datetime_t, date_t, time_t\n  implicit none\n  type(datetime_t), allocatable :: values(:)\n  integer :: i\n  values = [(datetime_t(date_t(2022, i, 8), time_t()), i = 1, 3)]\n  if (size(values) /= 3) error stop 1\n  if (values(2)%date%month /= 2) error stop 2\n  if (values(3)%date%day /= 8) error stop 3\n  if (values(1)%time%hour /= -1) error stop 4\n  print *, 'ok'\nend program\n",
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
        .expect("types compile failed to spawn");
    assert!(
        compile_types.status.success(),
        "types module should compile: {}",
        String::from_utf8_lossy(&compile_types.stderr)
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
        "imported same-name constructor should compile inside implied-do array constructors: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let out = dir.join("p");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            types_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("link failed to spawn");
    assert!(
        link.status.success(),
        "linked binary should build: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn local_narrow_integer_parameter_keeps_kind_for_generic_dispatch() {
    let src = write_program(
        "module m\n  implicit none\n  interface check\n    module procedure :: check_i1\n    module procedure :: check_i4\n  end interface\ncontains\n  subroutine check_i1(actual, expected)\n    integer(1), intent(in) :: actual, expected\n    if (actual /= expected) error stop 1\n    print *, 'ok'\n  end subroutine\n\n  subroutine check_i4(actual, expected)\n    integer(4), intent(in) :: actual, expected\n    error stop 2\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  integer, parameter :: i1 = selected_int_kind(2)\n  integer(i1), parameter :: expected = 7_i1\n  integer(i1) :: actual\n  actual = expected\n  call check(actual, expected)\nend program\n",
        "f90",
    );
    let out = unique_path("local_narrow_param_generic_dispatch", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("narrow integer parameter generic dispatch compile failed to spawn");
    assert!(
        compile.status.success(),
        "narrow integer parameter generic dispatch should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("narrow integer parameter generic dispatch run failed");
    assert!(
        run.status.success(),
        "narrow integer parameter generic dispatch should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected narrow integer parameter generic dispatch output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_narrow_integer_literal_keeps_kind_for_generic_dispatch() {
    let dir = unique_dir("imported_narrow_literal_generic_dispatch");
    let mod_src = write_program_in(
        &dir,
        "m.f90",
        "module m\n  implicit none\n  interface check\n    module procedure :: check_i1\n    module procedure :: check_i4\n  end interface\ncontains\n  subroutine check_i1(error, actual, expected, message, more)\n    integer, allocatable, intent(out) :: error(:)\n    integer(1), intent(in) :: actual, expected\n    character(len=*), intent(in), optional :: message\n    character(len=*), intent(in), optional :: more\n    if (actual /= expected) error stop 1\n    print *, 'i1'\n  end subroutine\n\n  subroutine check_i4(error, actual, expected, message, more)\n    integer, allocatable, intent(out) :: error(:)\n    integer(4), intent(in) :: actual, expected\n    character(len=*), intent(in), optional :: message\n    character(len=*), intent(in), optional :: more\n    error stop 2\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use m, only : check\n  implicit none\n  integer, parameter :: i1 = selected_int_kind(2)\n  integer, allocatable :: err(:)\n  integer(i1) :: val\n  val = 3_i1\n  call check(err, val, 3_i1)\nend program\n",
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
        .expect("imported narrow literal module compile failed to spawn");
    assert!(
        compile_mod.status.success(),
        "imported narrow literal module should compile: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("imported narrow literal main compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "imported narrow literal generic dispatch should compile: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("imported_narrow_literal_generic_dispatch.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            mod_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("imported narrow literal link failed to spawn");
    assert!(
        link.status.success(),
        "imported narrow literal generic dispatch should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("imported narrow literal generic dispatch run failed");
    assert!(
        run.status.success(),
        "imported narrow literal generic dispatch should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("i1"),
        "unexpected imported narrow literal generic dispatch output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_same_name_generics_merge_specifics_across_modules() {
    let dir = unique_dir("merged_imported_generics");
    let num_src = write_program_in(
        &dir,
        "num.f90",
        "module num\n  implicit none\n  interface to_string\n    module procedure :: int_to_string\n  end interface\ncontains\n  pure function int_to_string(value) result(string)\n    integer, intent(in) :: value\n    character(len=:), allocatable :: string\n    if (value == 7) then\n      string = 'seven'\n    else\n      string = 'other'\n    end if\n  end function\nend module\n",
    );
    let box_src = write_program_in(
        &dir,
        "box_mod.f90",
        "module box_mod\n  implicit none\n  type :: box_t\n    integer :: value = 0\n  end type\n  interface to_string\n    module procedure :: box_to_string\n  end interface\ncontains\n  pure function box_to_string(value) result(string)\n    type(box_t), intent(in) :: value\n    character(len=:), allocatable :: string\n    if (value%value == 42) then\n      string = 'box42'\n    else\n      string = 'box'\n    end if\n  end function\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use num\n  use box_mod, only : box_t, to_string\n  implicit none\n  type(box_t) :: value\n  character(len=:), allocatable :: string\n  value%value = 42\n  string = to_string(value)\n  if (string /= 'box42') error stop 1\n  print *, 'ok'\nend program\n",
    );

    let num_obj = dir.join("num.o");
    let compile_num = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            num_src.to_str().unwrap(),
            "-o",
            num_obj.to_str().unwrap(),
        ])
        .output()
        .expect("num compile failed to spawn");
    assert!(
        compile_num.status.success(),
        "numeric to_string module should compile: {}",
        String::from_utf8_lossy(&compile_num.stderr)
    );

    let box_obj = dir.join("box_mod.o");
    let compile_box = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            box_src.to_str().unwrap(),
            "-o",
            box_obj.to_str().unwrap(),
        ])
        .output()
        .expect("box compile failed to spawn");
    assert!(
        compile_box.status.success(),
        "derived-type to_string module should compile: {}",
        String::from_utf8_lossy(&compile_box.stderr)
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
        .expect("consumer compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "consumer compile should merge same-name imported generic specifics: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let out = dir.join("p");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            num_obj.to_str().unwrap(),
            box_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("link failed to spawn");
    assert!(
        link.status.success(),
        "linked binary should build: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn generic_dispatch_accepts_class_dummy_forwarding_to_char_specific_compile_only() {
    let src = write_program(
        "module m\n  implicit none\n  type :: key_t\n    character(len=:), allocatable :: key\n  end type\n  type :: node_t\n    integer :: tag = 0\n  end type\n  interface add_table\n    module procedure :: add_table_to_table\n    module procedure :: add_table_to_table_key\n  end interface\ncontains\n  subroutine add_table_to_table(table, key, ptr, stat)\n    class(node_t), target, intent(inout) :: table\n    character(len=*), intent(in) :: key\n    type(node_t), pointer, intent(out) :: ptr\n    integer, intent(out), optional :: stat\n    ptr => table\n    if (present(stat)) stat = len(key)\n  end subroutine\n\n  subroutine add_table_to_table_key(table, key, ptr, stat)\n    class(node_t), target, intent(inout) :: table\n    type(key_t), intent(in) :: key\n    type(node_t), pointer, intent(out) :: ptr\n    integer, intent(out), optional :: stat\n    call add_table(table, key%key, ptr, stat)\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(node_t), target :: table\n  type(key_t) :: key\n  type(node_t), pointer :: ptr\n  integer :: stat\n  key%key = 'abc'\n  call add_table(table, key, ptr, stat)\n  if (.not. associated(ptr)) error stop 1\n  if (stat /= 3) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("generic_class_forward_char", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("generic class-forwarding compile failed to spawn");
    assert!(
        compile.status.success(),
        "generic class-forwarding repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_generic_subroutine_with_optional_procedure_dummy_round_trips_through_amod() {
    let dir = unique_dir("imported_generic_optional_procedure_dummy");
    std::fs::create_dir_all(&dir).expect("create temp dir");

    let value_src = dir.join("value_m.f90");
    std::fs::write(
        &value_src,
        "module value_m\n  implicit none\n  type :: toml_key\n    character(len=:), allocatable :: key\n    integer :: origin = 0\n  end type\nend module\n",
    )
    .expect("write value module");

    let sort_src = dir.join("sort_m.f90");
    std::fs::write(
        &sort_src,
        "module sort_m\n  use value_m, only : toml_key\n  implicit none\n  interface sort\n    module procedure :: sort_keys\n  end interface\n  abstract interface\n    pure function compare_less(lhs, rhs) result(less)\n      import :: toml_key\n      type(toml_key), intent(in) :: lhs\n      type(toml_key), intent(in) :: rhs\n      logical :: less\n    end function\n  end interface\ncontains\n  subroutine sort_keys(list, idx, compare)\n    type(toml_key), intent(inout) :: list(:)\n    integer, intent(out), optional :: idx(:)\n    procedure(compare_less), optional :: compare\n    if (present(idx)) idx = 0\n  end subroutine\nend module\n",
    )
    .expect("write sort module");

    let main_src = dir.join("main.f90");
    std::fs::write(
        &main_src,
        "program p\n  use value_m, only : toml_key\n  use sort_m, only : sort\n  implicit none\n  type(toml_key) :: list(2)\n  list = [toml_key('0'), toml_key('1')]\n  call sort(list)\n  if (len(list(1)%key) /= 1) error stop 2\n  if (list(1)%origin /= 0) error stop 3\n  print *, 'ok'\nend program\n",
    )
    .expect("write main program");

    let value_obj = dir.join("value_m.o");
    let compile_value = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            value_src.to_str().unwrap(),
            "-o",
            value_obj.to_str().unwrap(),
        ])
        .output()
        .expect("value module compile failed to spawn");
    assert!(
        compile_value.status.success(),
        "value module should compile: {}",
        String::from_utf8_lossy(&compile_value.stderr)
    );

    let sort_obj = dir.join("sort_m.o");
    let compile_sort = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            sort_src.to_str().unwrap(),
            "-o",
            sort_obj.to_str().unwrap(),
        ])
        .output()
        .expect("sort module compile failed to spawn");
    assert!(
        compile_sort.status.success(),
        "sort module should compile: {}",
        String::from_utf8_lossy(&compile_sort.stderr)
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
        "imported generic with optional procedure dummy should compile: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let out = dir.join("p");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            main_obj.to_str().unwrap(),
            value_obj.to_str().unwrap(),
            sort_obj.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("link failed to spawn");
    assert!(
        link.status.success(),
        "linked binary should build: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "binary should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn same_name_generic_constructor_accepts_character_and_derived_array_actuals_compile_only() {
    let src = write_program(
        "module diag_mod\n  implicit none\n  type :: label_t\n    integer :: level = 0\n  end type\n  interface label_t\n    module procedure :: new_label\n  end interface\n  type :: diagnostic_t\n    integer :: level = 0\n    character(len=:), allocatable :: message\n    type(label_t), allocatable :: labels(:)\n  end type\n  interface diagnostic_t\n    module procedure :: new_diagnostic\n  end interface\ncontains\n  pure function new_label(level) result(new)\n    integer, intent(in) :: level\n    type(label_t) :: new\n    new%level = level\n  end function\n  pure function new_diagnostic(level, message, labels) result(new)\n    integer, intent(in) :: level\n    character(len=*), intent(in), optional :: message\n    type(label_t), intent(in), optional :: labels(:)\n    type(diagnostic_t) :: new\n    new%level = level\n    if (present(message)) new%message = message\n    if (present(labels)) new%labels = labels\n  end function\nend module\nprogram p\n  use diag_mod, only : diagnostic_t, label_t\n  implicit none\n  type(diagnostic_t) :: diag\n  diag = diagnostic_t(1, 'hello', [label_t(2)])\n  if (diag%level /= 1) error stop 1\n  if (diag%message /= 'hello') error stop 2\n  if (.not. allocated(diag%labels)) error stop 3\n  if (size(diag%labels) /= 1) error stop 4\n  if (diag%labels(1)%level /= 2) error stop 5\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("same_name_generic_ctor_char_array", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("same-name generic constructor compile failed to spawn");
    assert!(
        compile.status.success(),
        "same-name generic constructor with char and derived-array actuals should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn same_name_generic_interface_can_fall_back_to_structure_constructor() {
    let src = write_program(
        "module m\n  implicit none\n  type :: path_t\n    integer, allocatable :: path(:)\n  end type\n  interface path_t\n    module procedure :: new_path2\n    module procedure :: new_path3\n  end interface\ncontains\n  pure function new_path2(a, b) result(value)\n    character(*), intent(in) :: a, b\n    type(path_t) :: value\n    allocate(value%path(2))\n    value%path = [len_trim(a), len_trim(b)]\n  end function\n  pure function new_path3(a, b, c) result(value)\n    character(*), intent(in) :: a, b, c\n    type(path_t) :: value\n    allocate(value%path(3))\n    value%path = [len_trim(a), len_trim(b), len_trim(c)]\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  type(path_t) :: path\n  path = path_t([1, 2])\n  if (.not. allocated(path%path)) error stop 1\n  if (size(path%path) /= 2) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("same_name_generic_fallback_ctor", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("same-name generic fallback constructor compile failed to spawn");
    assert!(
        compile.status.success(),
        "same-name generic interface should fall back to structure constructor when specifics do not match: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn named_kind_prefixed_character_literal_compiles_and_runs() {
    let src = write_program(
        "program p\n  implicit none\n  integer, parameter :: tfc = 1\n  character(1, tfc) :: squote\n  squote = tfc_\"'\"\n  if (squote /= tfc_\"'\") error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("named_kind_prefixed_char_literal", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("named kind prefixed char literal compile failed to spawn");
    assert!(
        compile.status.success(),
        "named kind prefixed char literal should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("named kind prefixed char literal run failed");
    assert!(
        run.status.success(),
        "named kind prefixed char literal should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected named kind prefixed char literal output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn generic_interface_with_same_name_as_type_beats_structure_constructor() {
    let src = write_program(
        "module dt\n  implicit none\n  type :: pair\n    integer :: a = 0\n    integer :: b = 0\n  end type\n  interface pair\n    module procedure :: new_pair\n  end interface\ncontains\n  pure function new_pair(x) result(p)\n    integer, intent(in) :: x\n    type(pair) :: p\n    p%a = x\n    p%b = x + 1\n  end function\nend module\nprogram p\n  use dt\n  implicit none\n  type(pair) :: v\n  v = pair(7)\n  if (v%a /= 7) error stop 1\n  if (v%b /= 8) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("generic_type_name_beats_ctor", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("generic type-name compile failed to spawn");
    assert!(
        compile.status.success(),
        "generic type-name repro should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("generic type-name run failed");
    assert!(
        run.status.success(),
        "generic type-name repro should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected generic type-name output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn associated_on_procedure_pointer_component_runs() {
    let src = write_program(
        "module m\n  implicit none\n  abstract interface\n    subroutine cb(value)\n      integer, intent(in) :: value\n    end subroutine\n  end interface\n  type :: holder_t\n    procedure(cb), pointer, nopass :: handler => null()\n  end type\ncontains\n  subroutine assign_handler(holder)\n    type(holder_t), intent(inout) :: holder\n    holder%handler => target_handler\n  end subroutine\n\n  logical function has_handler(holder)\n    type(holder_t), intent(in) :: holder\n    has_handler = associated(holder%handler)\n  end function\n\n  subroutine target_handler(value)\n    integer, intent(in) :: value\n    if (value < 0) error stop 3\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(holder_t) :: holder\n  if (has_handler(holder)) error stop 1\n  call assign_handler(holder)\n  if (.not. has_handler(holder)) error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("associated_procptr_component", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("procptr component compile failed to spawn");
    assert!(
        compile.status.success(),
        "procptr component compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("procptr component run failed");
    assert!(
        run.status.success(),
        "procptr component run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected procptr component output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn procedure_pointer_component_call_updates_integer_argument() {
    let src = write_program(
        "module m\n  implicit none\n  abstract interface\n    subroutine cb(value)\n      integer, intent(inout) :: value\n    end subroutine\n  end interface\n  type :: holder_t\n    procedure(cb), pointer, nopass :: handler => null()\n  end type\ncontains\n  subroutine inc(value)\n    integer, intent(inout) :: value\n    value = value + 1\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(holder_t) :: holder\n  integer :: value\n  value = 0\n  holder%handler => inc\n  call holder%handler(value)\n  if (value /= 1) error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("procptr_component_call", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("procptr component call compile failed to spawn");
    assert!(
        compile.status.success(),
        "procptr component call compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("procptr component call run failed");
    assert!(
        run.status.success(),
        "procptr component call run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected procptr component call output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn procedure_pointer_component_actual_to_procedure_dummy_runs() {
    let src = write_program(
        "program p\n  implicit none\n  abstract interface\n    subroutine cb(value)\n      integer, intent(inout) :: value\n    end subroutine\n  end interface\n  type :: holder_t\n    procedure(cb), pointer, nopass :: handler => null()\n  end type\n  type(holder_t) :: holder\n  integer :: value\n  value = 0\n  holder%handler => inc\n  call run(holder%handler, value)\n  if (value /= 1) error stop 1\n  print *, 'ok'\ncontains\n  subroutine run(proc, value)\n    procedure(cb) :: proc\n    integer, intent(inout) :: value\n    call proc(value)\n  end subroutine\n\n  subroutine inc(value)\n    integer, intent(inout) :: value\n    value = value + 1\n  end subroutine\nend program\n",
        "f90",
    );
    let out = unique_path("procptr_component_actual_to_dummy", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("procptr component actual compile failed to spawn");
    assert!(
        compile.status.success(),
        "procptr component actual compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("procptr component actual run failed");
    assert!(
        run.status.success(),
        "procptr component actual run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected procptr component actual output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn structure_constructor_preserves_procptr_char_and_trailing_defaults() {
    let src = write_program(
        "program p\n  implicit none\n  abstract interface\n    subroutine cb()\n    end subroutine\n  end interface\n  type :: result_t\n    logical :: passed = .true.\n    character(len=16) :: message = ''\n  end type\n  type :: case_t\n    character(len=32) :: name = ''\n    procedure(cb), pointer, nopass :: proc => null()\n    type(result_t) :: result\n  end type\n  type(case_t) :: test\n  test = case_t('hello', local)\n  if (trim(test%name) /= 'hello') error stop 1\n  if (.not. associated(test%proc)) error stop 2\n  if (.not. test%result%passed) error stop 3\n  if (len_trim(test%result%message) /= 0) error stop 4\n  call test%proc()\n  print *, 'ok'\ncontains\n  subroutine local()\n  end subroutine\nend program\n",
        "f90",
    );
    let out = unique_path("structure_constructor_procptr_defaults", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("structure constructor procptr compile failed to spawn");
    assert!(
        compile.status.success(),
        "structure constructor procptr compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("structure constructor procptr run failed");
    assert!(
        run.status.success(),
        "structure constructor procptr run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected structure constructor procptr output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn array_constructor_of_function_results_preserves_procptr_components() {
    let src = write_program(
        "program p\n  implicit none\n  abstract interface\n    subroutine cb(value)\n      integer, intent(inout) :: value\n    end subroutine\n  end interface\n  type :: suite_t\n    character(len=:), allocatable :: name\n    procedure(cb), pointer, nopass :: collect => null()\n  end type\n  type(suite_t), allocatable :: suites(:)\n  integer :: value\n\n  value = 0\n  allocate(suites(0))\n  suites = [new_suite('build', inc), new_suite('lexer', inc)]\n  if (.not. associated(suites(1)%collect)) error stop 1\n  if (.not. associated(suites(2)%collect)) error stop 2\n  if (trim(suites(1)%name) /= 'build') error stop 3\n  if (trim(suites(2)%name) /= 'lexer') error stop 4\n  call suites(1)%collect(value)\n  call suites(2)%collect(value)\n  if (value /= 2) error stop 5\n  print *, 'ok'\ncontains\n  function new_suite(name, proc) result(suite)\n    character(len=*), intent(in) :: name\n    procedure(cb) :: proc\n    type(suite_t) :: suite\n    suite%name = name\n    suite%collect => proc\n  end function\n\n  subroutine inc(value)\n    integer, intent(inout) :: value\n    value = value + 1\n  end subroutine\nend program\n",
        "f90",
    );
    let out = unique_path("function_result_procptr_array_ctor", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("procptr array constructor compile failed to spawn");
    assert!(
        compile.status.success(),
        "procptr array constructor compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("procptr array constructor run failed");
    assert!(
        run.status.success(),
        "procptr array constructor run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected procptr array constructor output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_function_result_procptr_array_constructor_round_trips_and_runs() {
    let dir = unique_dir("imported_procptr_array_ctor");
    let callbacks_src = write_program_in(
        &dir,
        "callbacks.f90",
        "module callbacks\n  implicit none\n  abstract interface\n    subroutine cb(value)\n      integer, intent(inout) :: value\n    end subroutine\n  end interface\ncontains\n  subroutine inc(value)\n    integer, intent(inout) :: value\n    value = value + 1\n  end subroutine\nend module\n",
    );
    let suites_src = write_program_in(
        &dir,
        "suites.f90",
        "module suites\n  use callbacks, only : cb\n  implicit none\n  type :: suite_t\n    character(len=:), allocatable :: name\n    procedure(cb), pointer, nopass :: collect => null()\n  end type\ncontains\n  function new_suite(name, proc) result(suite)\n    character(len=*), intent(in) :: name\n    procedure(cb) :: proc\n    type(suite_t) :: suite\n    suite%name = name\n    suite%collect => proc\n  end function\n\n  subroutine run_suite(proc, value)\n    procedure(cb) :: proc\n    integer, intent(inout) :: value\n    call proc(value)\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use callbacks, only : inc\n  use suites, only : suite_t, new_suite, run_suite\n  implicit none\n  type(suite_t), allocatable :: suites_arr(:)\n  integer :: value\n  value = 0\n  allocate(suites_arr(0))\n  suites_arr = [new_suite('build', inc), new_suite('lexer', inc)]\n  if (.not. associated(suites_arr(1)%collect)) error stop 1\n  if (.not. associated(suites_arr(2)%collect)) error stop 2\n  call run_suite(suites_arr(1)%collect, value)\n  call run_suite(suites_arr(2)%collect, value)\n  if (value /= 2) error stop 3\n  print *, 'ok'\nend program\n",
    );

    let callbacks_obj = dir.join("callbacks.o");
    let compile_callbacks = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            callbacks_src.to_str().unwrap(),
            "-o",
            callbacks_obj.to_str().unwrap(),
        ])
        .output()
        .expect("callbacks compile spawn failed");
    assert!(
        compile_callbacks.status.success(),
        "callbacks compile should succeed: {}",
        String::from_utf8_lossy(&compile_callbacks.stderr)
    );

    let suites_obj = dir.join("suites.o");
    let compile_suites = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            suites_src.to_str().unwrap(),
            "-o",
            suites_obj.to_str().unwrap(),
        ])
        .output()
        .expect("suites compile spawn failed");
    assert!(
        compile_suites.status.success(),
        "suites compile should succeed: {}",
        String::from_utf8_lossy(&compile_suites.stderr)
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
        "main compile should succeed: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("imported_procptr_array_ctor.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            callbacks_obj.to_str().unwrap(),
            suites_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("imported procptr array ctor link spawn failed");
    assert!(
        link.status.success(),
        "imported procptr array ctor should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("imported procptr array ctor run failed");
    assert!(
        run.status.success(),
        "imported procptr array ctor run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected imported procptr array ctor output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_finalizable_procptr_array_constructor_round_trips_and_runs() {
    let dir = unique_dir("imported_finalizable_procptr_array_ctor");
    let callbacks_src = write_program_in(
        &dir,
        "callbacks.f90",
        "module callbacks\n  implicit none\n  abstract interface\n    subroutine cb(value)\n      integer, intent(inout) :: value\n    end subroutine\n  end interface\ncontains\n  subroutine inc(value)\n    integer, intent(inout) :: value\n    value = value + 1\n  end subroutine\nend module\n",
    );
    let suites_src = write_program_in(
        &dir,
        "suites.f90",
        "module suites\n  use callbacks, only : cb\n  implicit none\n  type :: suite_t\n    character(len=:), allocatable :: name\n    procedure(cb), pointer, nopass :: collect => null()\n  contains\n    final :: destroy_suite\n  end type\ncontains\n  subroutine destroy_suite(suite)\n    type(suite_t), intent(inout) :: suite\n    if (allocated(suite%name)) deallocate(suite%name)\n  end subroutine\n\n  function new_suite(name, proc) result(suite)\n    character(len=*), intent(in) :: name\n    procedure(cb) :: proc\n    type(suite_t) :: suite\n    suite%name = name\n    suite%collect => proc\n  end function\n\n  subroutine run_suite(proc, value)\n    procedure(cb) :: proc\n    integer, intent(inout) :: value\n    call proc(value)\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use callbacks, only : inc\n  use suites, only : suite_t, new_suite, run_suite\n  implicit none\n  type(suite_t), allocatable :: suites_arr(:)\n  integer :: value\n  value = 0\n  allocate(suites_arr(0))\n  suites_arr = [new_suite('build', inc), new_suite('lexer', inc)]\n  if (.not. associated(suites_arr(1)%collect)) error stop 1\n  if (.not. associated(suites_arr(2)%collect)) error stop 2\n  call run_suite(suites_arr(1)%collect, value)\n  call run_suite(suites_arr(2)%collect, value)\n  if (value /= 2) error stop 3\n  print *, 'ok'\nend program\n",
    );

    let callbacks_obj = dir.join("callbacks.o");
    let compile_callbacks = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            callbacks_src.to_str().unwrap(),
            "-o",
            callbacks_obj.to_str().unwrap(),
        ])
        .output()
        .expect("finalizable callbacks compile spawn failed");
    assert!(
        compile_callbacks.status.success(),
        "finalizable callbacks compile should succeed: {}",
        String::from_utf8_lossy(&compile_callbacks.stderr)
    );

    let suites_obj = dir.join("suites.o");
    let compile_suites = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            suites_src.to_str().unwrap(),
            "-o",
            suites_obj.to_str().unwrap(),
        ])
        .output()
        .expect("finalizable suites compile spawn failed");
    assert!(
        compile_suites.status.success(),
        "finalizable suites compile should succeed: {}",
        String::from_utf8_lossy(&compile_suites.stderr)
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
        .expect("finalizable main compile spawn failed");
    assert!(
        compile_main.status.success(),
        "finalizable main compile should succeed: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("imported_finalizable_procptr_array_ctor.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            callbacks_obj.to_str().unwrap(),
            suites_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("imported finalizable procptr array ctor link spawn failed");
    assert!(
        link.status.success(),
        "imported finalizable procptr array ctor should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("imported finalizable procptr array ctor run failed");
    assert!(
        run.status.success(),
        "imported finalizable procptr array ctor run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected imported finalizable procptr array ctor output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_finalizable_procptr_result_survives_finalizer_that_nulls_component() {
    let dir = unique_dir("imported_finalizer_nulls_procptr_result");
    let callbacks_src = write_program_in(
        &dir,
        "callbacks.f90",
        "module callbacks\n  implicit none\n  abstract interface\n    subroutine cb(value)\n      integer, intent(inout) :: value\n    end subroutine\n  end interface\ncontains\n  subroutine inc(value)\n    integer, intent(inout) :: value\n    value = value + 1\n  end subroutine\nend module\n",
    );
    let suites_src = write_program_in(
        &dir,
        "suites.f90",
        "module suites\n  use callbacks, only : cb\n  implicit none\n  type :: suite_t\n    character(len=:), allocatable :: name\n    procedure(cb), pointer, nopass :: collect => null()\n  contains\n    final :: destroy_suite\n  end type\ncontains\n  subroutine destroy_suite(suite)\n    type(suite_t), intent(inout) :: suite\n    if (allocated(suite%name)) deallocate(suite%name)\n    suite%collect => null()\n  end subroutine\n\n  function new_suite(name, proc) result(suite)\n    character(len=*), intent(in) :: name\n    procedure(cb) :: proc\n    type(suite_t) :: suite\n    suite%name = name\n    suite%collect => proc\n  end function\n\n  subroutine run_suite(proc, value)\n    procedure(cb) :: proc\n    integer, intent(inout) :: value\n    call proc(value)\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use callbacks, only : inc\n  use suites, only : suite_t, new_suite, run_suite\n  implicit none\n  type(suite_t), allocatable :: suites_arr(:)\n  integer :: value\n  value = 0\n  suites_arr = [new_suite('build', inc), new_suite('lexer', inc)]\n  if (.not. associated(suites_arr(1)%collect)) error stop 1\n  if (.not. associated(suites_arr(2)%collect)) error stop 2\n  call run_suite(suites_arr(1)%collect, value)\n  call run_suite(suites_arr(2)%collect, value)\n  if (value /= 2) error stop 3\n  print *, 'ok'\nend program\n",
    );

    let callbacks_obj = dir.join("callbacks.o");
    let compile_callbacks = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            callbacks_src.to_str().unwrap(),
            "-o",
            callbacks_obj.to_str().unwrap(),
        ])
        .output()
        .expect("nulling-finalizer callbacks compile spawn failed");
    assert!(
        compile_callbacks.status.success(),
        "nulling-finalizer callbacks compile should succeed: {}",
        String::from_utf8_lossy(&compile_callbacks.stderr)
    );

    let suites_obj = dir.join("suites.o");
    let compile_suites = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            suites_src.to_str().unwrap(),
            "-o",
            suites_obj.to_str().unwrap(),
        ])
        .output()
        .expect("nulling-finalizer suites compile spawn failed");
    assert!(
        compile_suites.status.success(),
        "nulling-finalizer suites compile should succeed: {}",
        String::from_utf8_lossy(&compile_suites.stderr)
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
        .expect("nulling-finalizer main compile spawn failed");
    assert!(
        compile_main.status.success(),
        "nulling-finalizer main compile should succeed: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("imported_finalizer_nulls_procptr_result.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            callbacks_obj.to_str().unwrap(),
            suites_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("nulling-finalizer procptr result link spawn failed");
    assert!(
        link.status.success(),
        "nulling-finalizer procptr result should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("nulling-finalizer procptr result run failed");
    assert!(
        run.status.success(),
        "nulling-finalizer procptr result run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected nulling-finalizer procptr result output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn derived_component_structure_constructor_defaults_initialize_locally() {
    let src = write_program(
        "program p\n  implicit none\n  integer, parameter :: i1 = selected_int_kind(2)\n  type :: color_code\n    integer(i1) :: style = -1_i1\n    integer(i1) :: bg = -1_i1\n    integer(i1) :: fg = -1_i1\n  end type\n  type :: color_output\n    type(color_code) :: bold = color_code(style=1_i1)\n    type(color_code) :: blue = color_code(fg=4_i1)\n  end type\n  type(color_output) :: color\n  if (color%bold%style /= 1_i1) error stop 1\n  if (color%bold%bg /= -1_i1) error stop 2\n  if (color%blue%fg /= 4_i1) error stop 3\n  if (.not. anycolor(color%bold)) error stop 4\n  if (.not. anycolor(color%blue)) error stop 5\n  print *, 'ok'\ncontains\n  pure function anycolor(code) result(flag)\n    type(color_code), intent(in) :: code\n    logical :: flag\n    flag = code%fg >= 0 .or. code%bg >= 0 .or. code%style >= 0\n  end function\nend program\n",
        "f90",
    );
    let out = unique_path("derived_component_ctor_defaults_local", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("derived component ctor default compile spawn failed");
    assert!(
        compile.status.success(),
        "derived component ctor defaults should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "derived component ctor defaults should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected derived component ctor default output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn runtime_structure_constructor_keyword_args_preserve_named_fields_and_defaults() {
    let src = write_program(
        "program p\n  implicit none\n  integer, parameter :: i1 = selected_int_kind(2)\n  type :: color_code\n    integer(i1) :: style = -1_i1\n    integer(i1) :: bg = -1_i1\n    integer(i1) :: fg = -1_i1\n  end type\n  type(color_code) :: code\n  code = color_code(style=8_i1)\n  if (code%style /= 8_i1) error stop 1\n  if (code%bg /= -1_i1) error stop 2\n  if (code%fg /= -1_i1) error stop 3\n  code = color_code(fg=3_i1)\n  if (code%style /= -1_i1) error stop 4\n  if (code%bg /= -1_i1) error stop 5\n  if (code%fg /= 3_i1) error stop 6\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("runtime_structure_ctor_keywords", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("runtime structure ctor keyword compile spawn failed");
    assert!(
        compile.status.success(),
        "runtime structure ctor keyword case should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "runtime structure ctor keyword case should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected runtime structure ctor keyword output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_derived_component_structure_constructor_defaults_round_trip_through_amod_and_run() {
    let dir = unique_dir("derived_component_ctor_defaults_amod");
    let mod_src = write_program_in(
        &dir,
        "color_mod.f90",
        "module color_mod\n  implicit none\n  integer, parameter :: i1 = selected_int_kind(2)\n  type :: color_code\n    integer(i1) :: style = -1_i1\n    integer(i1) :: bg = -1_i1\n    integer(i1) :: fg = -1_i1\n  end type\n  type, public :: color_output\n    type(color_code) :: bold = color_code(style=1_i1)\n    type(color_code) :: blue = color_code(fg=4_i1)\n  end type\ncontains\n  pure function anycolor(code) result(flag)\n    type(color_code), intent(in) :: code\n    logical :: flag\n    flag = code%fg >= 0 .or. code%bg >= 0 .or. code%style >= 0\n  end function\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use color_mod, only : color_output, color_code\n  implicit none\n  type(color_output) :: color\n  if (color%bold%style /= 1) error stop 1\n  if (color%blue%fg /= 4) error stop 2\n  if (.not. anycolor(color%bold)) error stop 3\n  if (.not. anycolor(color%blue)) error stop 4\n  print *, 'ok'\ncontains\n  pure function anycolor(code) result(flag)\n    type(color_code), intent(in) :: code\n    logical :: flag\n    flag = code%fg >= 0 .or. code%bg >= 0 .or. code%style >= 0\n  end function\nend program\n",
    );

    let mod_obj = dir.join("color_mod.o");
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
        .expect("color module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "color module should compile: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let amod = dir.join("color_mod.amod");
    let amod_text = std::fs::read_to_string(&amod).expect("missing color_mod.amod");
    assert!(
        amod_text.contains("@init=exprhex:"),
        "derived component ctor defaults should be exported to .amod: {}",
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
        "main should compile against imported derived ctor defaults: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("derived_component_ctor_defaults.bin");
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
        "derived component ctor defaults objects should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run failed");
    assert!(
        run.status.success(),
        "imported derived component ctor defaults should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected imported derived component ctor default output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_descriptor_callback_procptr_array_constructor_round_trips_and_runs() {
    let dir = unique_dir("imported_descriptor_procptr_array_ctor");
    let callbacks_src = write_program_in(
        &dir,
        "callbacks.f90",
        "module callbacks\n  implicit none\n  type :: unittest_t\n    integer :: value = 0\n  end type\n  abstract interface\n    subroutine collect_interface(testsuite)\n      import :: unittest_t\n      type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    end subroutine\n  end interface\ncontains\n  subroutine collect_build(testsuite)\n    type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    allocate(testsuite(1))\n    testsuite(1)%value = 7\n  end subroutine\nend module\n",
    );
    let suites_src = write_program_in(
        &dir,
        "suites.f90",
        "module suites\n  use callbacks, only : unittest_t, collect_interface\n  implicit none\n  type :: suite_t\n    character(len=:), allocatable :: name\n    procedure(collect_interface), pointer, nopass :: collect => null()\n  contains\n    final :: destroy_suite\n  end type\ncontains\n  subroutine destroy_suite(suite)\n    type(suite_t), intent(inout) :: suite\n    if (allocated(suite%name)) deallocate(suite%name)\n  end subroutine\n\n  function new_suite(name, proc) result(suite)\n    character(len=*), intent(in) :: name\n    procedure(collect_interface) :: proc\n    type(suite_t) :: suite\n    suite%name = name\n    suite%collect => proc\n  end function\n\n  subroutine run_suite(proc, value)\n    procedure(collect_interface) :: proc\n    integer, intent(out) :: value\n    type(unittest_t), allocatable :: tests(:)\n    call proc(tests)\n    if (.not. allocated(tests)) error stop 9\n    value = tests(1)%value\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use callbacks, only : collect_build\n  use suites, only : suite_t, new_suite, run_suite\n  implicit none\n  type(suite_t), allocatable :: suites_arr(:)\n  integer :: value\n  value = 0\n  allocate(suites_arr(0))\n  suites_arr = [new_suite('build', collect_build)]\n  if (.not. associated(suites_arr(1)%collect)) error stop 1\n  call run_suite(suites_arr(1)%collect, value)\n  if (value /= 7) error stop 2\n  print *, 'ok'\nend program\n",
    );

    let callbacks_obj = dir.join("callbacks.o");
    let compile_callbacks = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            callbacks_src.to_str().unwrap(),
            "-o",
            callbacks_obj.to_str().unwrap(),
        ])
        .output()
        .expect("descriptor callbacks compile spawn failed");
    assert!(
        compile_callbacks.status.success(),
        "descriptor callbacks compile should succeed: {}",
        String::from_utf8_lossy(&compile_callbacks.stderr)
    );
    let callbacks_amod = std::fs::read_to_string(dir.join("callbacks.amod"))
        .expect("descriptor callbacks module should emit callbacks.amod");
    assert!(
        callbacks_amod.contains("@subroutine collect_interface")
            && callbacks_amod.contains(
                "@arg testsuite : type(unittest_t), intent(out), descriptor, allocatable"
            ),
        "abstract interface descriptor dummy should survive into .amod: {}",
        callbacks_amod
    );

    let suites_obj = dir.join("suites.o");
    let compile_suites = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            suites_src.to_str().unwrap(),
            "-o",
            suites_obj.to_str().unwrap(),
        ])
        .output()
        .expect("descriptor suites compile spawn failed");
    assert!(
        compile_suites.status.success(),
        "descriptor suites compile should succeed: {}",
        String::from_utf8_lossy(&compile_suites.stderr)
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
        .expect("descriptor main compile spawn failed");
    assert!(
        compile_main.status.success(),
        "descriptor main compile should succeed: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("imported_descriptor_procptr_array_ctor.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            callbacks_obj.to_str().unwrap(),
            suites_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("descriptor procptr array ctor link spawn failed");
    assert!(
        link.status.success(),
        "descriptor procptr array ctor should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("descriptor procptr array ctor run failed");
    assert!(
        run.status.success(),
        "descriptor procptr array ctor run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected descriptor procptr array ctor output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_descriptor_callback_many_array_constructor_entries_preserve_procptr_components() {
    let dir = unique_dir("imported_descriptor_procptr_many_ctor");
    let callbacks_src = write_program_in(
        &dir,
        "callbacks.f90",
        "module callbacks\n  implicit none\n  type :: unittest_t\n    integer :: value = 0\n  end type\n  abstract interface\n    subroutine collect_interface(testsuite)\n      import :: unittest_t\n      type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    end subroutine\n  end interface\ncontains\n  subroutine collect_1(testsuite)\n    type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    allocate(testsuite(1))\n    testsuite(1)%value = 1\n  end subroutine\n  subroutine collect_2(testsuite)\n    type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    allocate(testsuite(1))\n    testsuite(1)%value = 2\n  end subroutine\n  subroutine collect_3(testsuite)\n    type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    allocate(testsuite(1))\n    testsuite(1)%value = 3\n  end subroutine\n  subroutine collect_4(testsuite)\n    type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    allocate(testsuite(1))\n    testsuite(1)%value = 4\n  end subroutine\n  subroutine collect_5(testsuite)\n    type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    allocate(testsuite(1))\n    testsuite(1)%value = 5\n  end subroutine\n  subroutine collect_6(testsuite)\n    type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    allocate(testsuite(1))\n    testsuite(1)%value = 6\n  end subroutine\nend module\n",
    );
    let suites_src = write_program_in(
        &dir,
        "suites.f90",
        "module suites\n  use callbacks, only : unittest_t, collect_interface\n  implicit none\n  type :: suite_t\n    character(len=:), allocatable :: name\n    procedure(collect_interface), pointer, nopass :: collect => null()\n  contains\n    final :: destroy_suite\n  end type\ncontains\n  subroutine destroy_suite(suite)\n    type(suite_t), intent(inout) :: suite\n    if (allocated(suite%name)) deallocate(suite%name)\n  end subroutine\n\n  function new_suite(name, proc) result(suite)\n    character(len=*), intent(in) :: name\n    procedure(collect_interface) :: proc\n    type(suite_t) :: suite\n    suite%name = name\n    suite%collect => proc\n  end function\n\n  subroutine run_suite(proc, value)\n    procedure(collect_interface) :: proc\n    integer, intent(out) :: value\n    type(unittest_t), allocatable :: tests(:)\n    call proc(tests)\n    if (.not. allocated(tests)) error stop 9\n    value = tests(1)%value\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use callbacks, only : collect_1, collect_2, collect_3, collect_4, collect_5, collect_6\n  use suites, only : suite_t, new_suite, run_suite\n  implicit none\n  type(suite_t), allocatable :: suites_arr(:)\n  integer :: i, value, total\n  total = 0\n  allocate(suites_arr(0))\n  suites_arr = [ &
 & new_suite('one', collect_1), &
 & new_suite('two', collect_2), &
 & new_suite('three', collect_3), &
 & new_suite('four', collect_4), &
 & new_suite('five', collect_5), &
 & new_suite('six', collect_6) &
 & ]\n  do i = 1, size(suites_arr)\n    if (.not. associated(suites_arr(i)%collect)) error stop 100 + i\n    call run_suite(suites_arr(i)%collect, value)\n    total = total + value\n  end do\n  if (total /= 21) error stop 200\n  print *, 'ok'\nend program\n",
    );

    let callbacks_obj = dir.join("callbacks.o");
    let compile_callbacks = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            callbacks_src.to_str().unwrap(),
            "-o",
            callbacks_obj.to_str().unwrap(),
        ])
        .output()
        .expect("many callbacks compile spawn failed");
    assert!(
        compile_callbacks.status.success(),
        "many callbacks compile should succeed: {}",
        String::from_utf8_lossy(&compile_callbacks.stderr)
    );

    let suites_obj = dir.join("suites.o");
    let compile_suites = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            suites_src.to_str().unwrap(),
            "-o",
            suites_obj.to_str().unwrap(),
        ])
        .output()
        .expect("many suites compile spawn failed");
    assert!(
        compile_suites.status.success(),
        "many suites compile should succeed: {}",
        String::from_utf8_lossy(&compile_suites.stderr)
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
        .expect("many main compile spawn failed");
    assert!(
        compile_main.status.success(),
        "many main compile should succeed: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("imported_descriptor_procptr_many_ctor.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            callbacks_obj.to_str().unwrap(),
            suites_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("many descriptor procptr array ctor link spawn failed");
    assert!(
        link.status.success(),
        "many descriptor procptr array ctor should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("many descriptor procptr array ctor run failed");
    assert!(
        run.status.success(),
        "many descriptor procptr array ctor run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected many descriptor procptr array ctor output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_cross_module_descriptor_callback_array_constructor_preserves_procptr_components() {
    let dir = unique_dir("imported_cross_module_descriptor_procptr_ctor");
    let iface_src = write_program_in(
        &dir,
        "iface.f90",
        "module iface_mod\n  implicit none\n  type :: unittest_t\n    integer :: value = 0\n  end type\n  abstract interface\n    subroutine collect_interface(testsuite)\n      import :: unittest_t\n      type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    end subroutine\n  end interface\nend module\n",
    );
    let suites_src = write_program_in(
        &dir,
        "suites.f90",
        "module suites\n  use iface_mod, only : unittest_t, collect_interface\n  implicit none\n  type :: suite_t\n    character(len=:), allocatable :: name\n    procedure(collect_interface), pointer, nopass :: collect => null()\n  contains\n    final :: destroy_suite\n  end type\ncontains\n  subroutine destroy_suite(suite)\n    type(suite_t), intent(inout) :: suite\n    if (allocated(suite%name)) deallocate(suite%name)\n  end subroutine\n\n  function new_suite(name, proc) result(suite)\n    character(len=*), intent(in) :: name\n    procedure(collect_interface) :: proc\n    type(suite_t) :: suite\n    suite%name = name\n    suite%collect => proc\n  end function\n\n  subroutine run_suite(proc, value)\n    procedure(collect_interface) :: proc\n    integer, intent(out) :: value\n    type(unittest_t), allocatable :: tests(:)\n    call proc(tests)\n    if (.not. allocated(tests)) error stop 9\n    value = tests(1)%value\n  end subroutine\nend module\n",
    );
    let providers_src = write_program_in(
        &dir,
        "providers.f90",
        "module providers\n  use iface_mod, only : unittest_t\n  implicit none\ncontains\n  subroutine collect_1(testsuite)\n    type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    allocate(testsuite(1))\n    testsuite(1)%value = 1\n  end subroutine\n  subroutine collect_2(testsuite)\n    type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    allocate(testsuite(1))\n    testsuite(1)%value = 2\n  end subroutine\n  subroutine collect_3(testsuite)\n    type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    allocate(testsuite(1))\n    testsuite(1)%value = 3\n  end subroutine\n  subroutine collect_4(testsuite)\n    type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    allocate(testsuite(1))\n    testsuite(1)%value = 4\n  end subroutine\n  subroutine collect_5(testsuite)\n    type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    allocate(testsuite(1))\n    testsuite(1)%value = 5\n  end subroutine\n  subroutine collect_6(testsuite)\n    type(unittest_t), allocatable, intent(out) :: testsuite(:)\n    allocate(testsuite(1))\n    testsuite(1)%value = 6\n  end subroutine\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use providers, only : collect_1, collect_2, collect_3, collect_4, collect_5, collect_6\n  use suites, only : suite_t, new_suite, run_suite\n  implicit none\n  type(suite_t), allocatable :: suites_arr(:)\n  integer :: i, value, total\n  total = 0\n  allocate(suites_arr(0))\n  suites_arr = [ &
 & new_suite('one', collect_1), &
 & new_suite('two', collect_2), &
 & new_suite('three', collect_3), &
 & new_suite('four', collect_4), &
 & new_suite('five', collect_5), &
 & new_suite('six', collect_6) &
 & ]\n  do i = 1, size(suites_arr)\n    if (.not. associated(suites_arr(i)%collect)) error stop 100 + i\n    call run_suite(suites_arr(i)%collect, value)\n    total = total + value\n  end do\n  if (total /= 21) error stop 200\n  print *, 'ok'\nend program\n",
    );

    let iface_obj = dir.join("iface.o");
    let compile_iface = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            iface_src.to_str().unwrap(),
            "-o",
            iface_obj.to_str().unwrap(),
        ])
        .output()
        .expect("cross-module iface compile spawn failed");
    assert!(
        compile_iface.status.success(),
        "cross-module iface compile should succeed: {}",
        String::from_utf8_lossy(&compile_iface.stderr)
    );

    let providers_obj = dir.join("providers.o");
    let compile_providers = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            providers_src.to_str().unwrap(),
            "-o",
            providers_obj.to_str().unwrap(),
        ])
        .output()
        .expect("cross-module providers compile spawn failed");
    assert!(
        compile_providers.status.success(),
        "cross-module providers compile should succeed: {}",
        String::from_utf8_lossy(&compile_providers.stderr)
    );

    let suites_obj = dir.join("suites.o");
    let compile_suites = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            suites_src.to_str().unwrap(),
            "-o",
            suites_obj.to_str().unwrap(),
        ])
        .output()
        .expect("cross-module suites compile spawn failed");
    assert!(
        compile_suites.status.success(),
        "cross-module suites compile should succeed: {}",
        String::from_utf8_lossy(&compile_suites.stderr)
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
        .expect("cross-module main compile spawn failed");
    assert!(
        compile_main.status.success(),
        "cross-module main compile should succeed: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("imported_cross_module_descriptor_procptr_ctor.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            iface_obj.to_str().unwrap(),
            providers_obj.to_str().unwrap(),
            suites_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("cross-module descriptor procptr ctor link spawn failed");
    assert!(
        link.status.success(),
        "cross-module descriptor procptr ctor should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("cross-module descriptor procptr ctor run failed");
    assert!(
        run.status.success(),
        "cross-module descriptor procptr ctor run failed: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected cross-module descriptor procptr ctor output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn public_type_emits_private_nested_type_layouts_needed_cross_tu() {
    let dir = unique_dir("private_nested_layout");
    let mod_src = write_program_in(
        &dir,
        "tree_mod.f90",
        "module tree_mod\n  implicit none\n  private\n  public :: tree_state_t, init_tree\n  type :: node_t\n    integer :: value = 0\n    type(node_t), pointer :: parent => null()\n  end type\n  type :: selectable_t\n    logical :: is_directory = .false.\n    type(node_t), pointer :: node => null()\n  end type\n  type :: tree_state_t\n    type(selectable_t), allocatable :: selectable_files(:)\n  end type\ncontains\n  subroutine init_tree(state)\n    type(tree_state_t), intent(out) :: state\n    allocate(state%selectable_files(1))\n    state%selectable_files(1)%is_directory = .true.\n    allocate(state%selectable_files(1)%node)\n    state%selectable_files(1)%node%value = 7\n  end subroutine\nend module\n",
    );
    let user_src = write_program_in(
        &dir,
        "user.f90",
        "program p\n  use tree_mod\n  implicit none\n  type(tree_state_t) :: state\n  call init_tree(state)\n  if (.not. allocated(state%selectable_files)) error stop 1\n  if (.not. state%selectable_files(1)%is_directory) error stop 2\n  if (.not. associated(state%selectable_files(1)%node)) error stop 3\n  if (state%selectable_files(1)%node%value /= 7) error stop 4\n  if (associated(state%selectable_files(1)%node%parent)) error stop 5\n  print *, 'ok'\nend program\n",
    );

    let mod_obj = dir.join("tree_mod.o");
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
        .expect("tree module compile spawn failed");
    assert!(
        compile_mod.status.success(),
        "tree module should compile: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let amod = dir.join("tree_mod.amod");
    let amod_text = std::fs::read_to_string(&amod).expect("missing tree_mod.amod");
    assert!(
        amod_text.contains("@type selectable_t") && amod_text.contains("@type node_t"),
        "public nested type closure should be emitted to .amod: {}",
        amod_text
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
        .expect("user compile spawn failed");
    assert!(
        compile_user.status.success(),
        "user compile should succeed with imported private nested layouts: {}",
        String::from_utf8_lossy(&compile_user.stderr)
    );

    let exe = dir.join("tree_nested.bin");
    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            mod_obj.to_str().unwrap(),
            user_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "private nested layout test should link: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run spawn failed");
    assert!(
        run.status.success(),
        "private nested layout test should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected private nested layout output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn module_procedure_actual_is_not_mislowered_as_internal_proc() {
    let src = write_program(
        "module callbacks\n  implicit none\n  abstract interface\n    subroutine cb(value)\n      integer, intent(in) :: value\n    end subroutine\n  end interface\ncontains\n  subroutine register_only(handler)\n    procedure(cb) :: handler\n  end subroutine\n\n  subroutine drive()\n    call register_only(wrapper)\n    print *, 'ok'\n  end subroutine\n\n  subroutine wrapper(value)\n    integer, intent(in) :: value\n    if (value < 0) error stop 1\n  end subroutine\nend module\nprogram p\n  use callbacks\n  implicit none\n  call drive()\nend program\n",
        "f90",
    );
    let out = unique_path("module_proc_actual_not_internal", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("module procedure actual compile failed to spawn");
    assert!(
        compile.status.success(),
        "module procedure actual should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("module procedure actual run failed");
    assert!(
        run.status.success(),
        "module procedure actual should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected module procedure actual output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn overloaded_concat_with_char_dummy_and_hidden_string_result_runs() {
    let src = write_program(
        "module colors\n  implicit none\n  type :: color_t\n    integer :: code = 0\n  end type\n  interface operator(//)\n    module procedure concat_color_left\n  end interface\ncontains\n  pure logical function anycolor(code)\n    type(color_t), intent(in) :: code\n    anycolor = code%code > 0\n  end function\n\n  pure function escape_color(code) result(str)\n    type(color_t), intent(in) :: code\n    character(len=:), allocatable :: str\n    if (anycolor(code)) then\n      str = '[X]'\n    else\n      str = ''\n    end if\n  end function\n\n  pure function concat_color_left(lval, code) result(str)\n    character(len=*), intent(in) :: lval\n    type(color_t), intent(in) :: code\n    character(len=:), allocatable :: str\n    str = lval // escape_color(code)\n  end function\nend module\nprogram p\n  use colors\n  implicit none\n  type(color_t) :: color\n  character(len=:), allocatable :: msg\n  color%code = 1\n  msg = 'A' // color\n  if (msg /= 'A[X]') error stop 1\n  print *, trim(msg)\nend program\n",
        "f90",
    );
    let out = unique_path("operator_concat_hidden_string_result", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("overloaded concat compile failed to spawn");
    assert!(
        compile.status.success(),
        "overloaded concat should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("overloaded concat run failed");
    assert!(
        run.status.success(),
        "overloaded concat should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("A[X]"),
        "unexpected overloaded concat output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn overloaded_concat_with_color_on_left_preserves_char_rhs() {
    let src = write_program(
        "module colors\n  implicit none\n  type :: color_t\n    integer :: code = 0\n  end type\n  interface operator(//)\n    module procedure concat_color_left\n    module procedure concat_color_right\n  end interface\ncontains\n  pure function concat_color_left(lval, code) result(str)\n    character(len=*), intent(in) :: lval\n    type(color_t), intent(in) :: code\n    character(len=:), allocatable :: str\n    if (code%code > 0) then\n      str = lval // '[X]'\n    else\n      str = lval\n    end if\n  end function\n\n  pure function concat_color_right(code, rval) result(str)\n    type(color_t), intent(in) :: code\n    character(len=*), intent(in) :: rval\n    character(len=:), allocatable :: str\n    if (code%code > 0) then\n      str = '[X]' // rval\n    else\n      str = rval\n    end if\n  end function\nend module\nprogram p\n  use colors\n  implicit none\n  type(color_t) :: color\n  character(len=:), allocatable :: msg, plain\n  color%code = 1\n  plain = 'A'\n  msg = color // plain\n  if (msg /= '[X]A') error stop 1\n  msg = plain // color\n  if (msg /= 'A[X]') error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("operator_concat_color_left", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("left-handed overloaded concat compile failed to spawn");
    assert!(
        compile.status.success(),
        "left-handed overloaded concat should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("left-handed overloaded concat run failed");
    assert!(
        run.status.success(),
        "left-handed overloaded concat should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected left-handed overloaded concat output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn overloaded_concat_with_component_char_actual_preserves_rhs() {
    let src = write_program(
        "module colors\n  implicit none\n  type :: color_t\n    integer :: code = 0\n  end type\n  type :: palette_t\n    type(color_t) :: blue = color_t(1)\n    type(color_t) :: reset = color_t(0)\n  end type\n  type :: label_t\n    character(len=:), allocatable :: text\n  end type\n  type(palette_t), save :: palette\n  interface operator(//)\n    module procedure concat_color_left\n    module procedure concat_color_right\n  end interface\ncontains\n  pure function concat_color_left(lval, code) result(str)\n    character(len=*), intent(in) :: lval\n    type(color_t), intent(in) :: code\n    character(len=:), allocatable :: str\n    if (code%code > 0) then\n      str = lval // '[X]'\n    else\n      str = lval\n    end if\n  end function\n\n  pure function concat_color_right(code, rval) result(str)\n    type(color_t), intent(in) :: code\n    character(len=*), intent(in) :: rval\n    character(len=:), allocatable :: str\n    if (code%code > 0) then\n      str = '[X]' // rval\n    else\n      str = rval\n    end if\n  end function\nend module\nprogram p\n  use colors\n  implicit none\n  type(label_t) :: label\n  character(len=:), allocatable :: msg\n  label%text = 'A'\n  msg = palette%blue // label%text\n  if (msg /= '[X]A') error stop 1\n  msg = label%text // palette%reset\n  if (msg /= 'A') error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("operator_concat_component_char_actual", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("component char overloaded concat compile failed to spawn");
    assert!(
        compile.status.success(),
        "component char overloaded concat should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("component char overloaded concat run failed");
    assert!(
        run.status.success(),
        "component char overloaded concat should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected component char overloaded concat output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn module_character_parameter_still_uses_intrinsic_concat_with_user_defined_slash_slash() {
    let src = write_program(
        "module m\n  implicit none\n  type :: color_code\n    integer :: style = -1\n  end type\n  interface operator(//)\n    module procedure :: concat_color_left\n  end interface\n  character(len=*), parameter :: newline = new_line('a')\ncontains\n  pure function concat_color_left(lval, code) result(str)\n    character(len=*), intent(in) :: lval\n    type(color_code), intent(in) :: code\n    character(len=:), allocatable :: str\n    if (code%style >= 0) then\n      str = lval // 'X'\n    else\n      str = lval // 'Y'\n    end if\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  character(len=:), allocatable :: s\n  s = 'A' // newline // 'B'\n  if (len(s) /= 3) error stop 1\n  if (s /= 'A' // new_line('a') // 'B') error stop 2\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("module_char_param_intrinsic_concat", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("module char param intrinsic concat compile spawn failed");
    assert!(
        compile.status.success(),
        "module char param intrinsic concat should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out).output().expect("run failed");
    assert!(
        run.status.success(),
        "module char param intrinsic concat should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected module char param intrinsic concat output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn module_global_component_overloaded_concat_preserves_nested_character_actual_len() {
    let src = write_program(
        "module m\n  implicit none\n  integer, parameter :: i1 = selected_int_kind(2)\n  type :: color_code\n    integer(i1) :: style = -1_i1\n    integer(i1) :: bg = -1_i1\n    integer(i1) :: fg = -1_i1\n  end type\n  type :: color_output\n    type(color_code) :: dim = color_code(style=2_i1)\n    type(color_code) :: reset = color_code()\n    type(color_code) :: blue = color_code(fg=4_i1)\n  end type\n  type(color_output), protected :: color\n  interface operator(//)\n    module procedure :: concat_color_left\n    module procedure :: concat_color_right\n  end interface\ncontains\n  pure function concat_color_left(lval, code) result(str)\n    character(len=*), intent(in) :: lval\n    type(color_code), intent(in) :: code\n    character(len=:), allocatable :: str\n    if (anycolor(code)) then\n      str = lval // 'X'\n    else\n      str = lval // 'Y'\n    end if\n  end function\n  pure function concat_color_right(code, rval) result(str)\n    type(color_code), intent(in) :: code\n    character(len=*), intent(in) :: rval\n    character(len=:), allocatable :: str\n    if (anycolor(code)) then\n      str = 'Z' // rval\n    else\n      str = 'W' // rval\n    end if\n  end function\n  pure function anycolor(code) result(flag)\n    type(color_code), intent(in) :: code\n    logical :: flag\n    flag = code%fg >= 0 .or. code%bg >= 0 .or. code%style >= 0\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  character(len=:), allocatable :: s\n  s = 'A' // color%dim // 'B' // color%reset // 'C' // color%blue\n  if (s /= 'AXBYCX') error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("module_global_component_overloaded_concat_nested_len", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("module global component overloaded concat compile failed to spawn");
    assert!(
        compile.status.success(),
        "module global component overloaded concat should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("module global component overloaded concat run failed");
    assert!(
        run.status.success(),
        "module global component overloaded concat should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected module global component overloaded concat output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn chained_hidden_result_concat_with_stub_ansi_codes_runs() {
    let src = write_program(
        "module m\n  implicit none\n  integer, parameter :: i1 = selected_int_kind(2)\n  type :: ansi_code\n    integer(i1) :: style = -1_i1\n    integer(i1) :: bg = -1_i1\n    integer(i1) :: fg = -1_i1\n  end type\n  type :: term\n    type(ansi_code) :: reset = ansi_code()\n    type(ansi_code) :: bold = ansi_code()\n    type(ansi_code) :: blue = ansi_code(fg=4_i1)\n  end type\n  interface operator(+)\n    module procedure :: add\n  end interface\n  interface operator(//)\n    module procedure :: concat_left\n    module procedure :: concat_right\n  end interface\ncontains\n  pure function add(lval, rval) result(code)\n    type(ansi_code), intent(in) :: lval, rval\n    type(ansi_code) :: code\n    code%style = merge(rval%style, lval%style, rval%style >= 0)\n    code%fg = merge(rval%fg, lval%fg, rval%fg >= 0)\n    code%bg = merge(rval%bg, lval%bg, rval%bg >= 0)\n  end function\n  pure function anycolor(code) result(flag)\n    type(ansi_code), intent(in) :: code\n    logical :: flag\n    flag = code%fg >= 0 .or. code%bg >= 0 .or. code%style >= 0\n  end function\n  pure function escape(code) result(str)\n    type(ansi_code), intent(in) :: code\n    character(len=:), allocatable :: str\n    if (anycolor(code)) then\n      str = '[C]'\n    else\n      str = ''\n    end if\n  end function\n  pure function concat_left(lval, code) result(str)\n    character(len=*), intent(in) :: lval\n    type(ansi_code), intent(in) :: code\n    character(len=:), allocatable :: str\n    str = lval // escape(code)\n  end function\n  pure function concat_right(code, rval) result(str)\n    type(ansi_code), intent(in) :: code\n    character(len=*), intent(in) :: rval\n    character(len=:), allocatable :: str\n    str = escape(code) // rval\n  end function\n  pure function level_name(level, color) result(string)\n    integer, intent(in) :: level\n    type(term), intent(in) :: color\n    character(len=:), allocatable :: string\n    if (level == 0) then\n      string = color%bold + color%blue // 'error' // color%reset\n    else\n      string = color%bold + color%blue // 'unknown' // color%reset\n    end if\n  end function\n  pure function render_message(level, message, color) result(string)\n    integer, intent(in) :: level\n    character(len=*), intent(in) :: message\n    type(term), intent(in) :: color\n    character(len=:), allocatable :: string\n    string = level_name(level, color) // color%bold // ': ' // message // color%reset\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  type(term) :: color\n  character(len=:), allocatable :: msg\n  msg = render_message(0, 'Invalid syntax', color)\n  if (msg /= '[C]error: Invalid syntax') error stop 1\n  print *, trim(msg)\nend program\n",
        "f90",
    );
    let out = unique_path("chained_hidden_result_concat_stub_ansi", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("stub ansi concat chain compile failed to spawn");
    assert!(
        compile.status.success(),
        "stub ansi concat chain should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("stub ansi concat chain run failed");
    assert!(
        run.status.success(),
        "stub ansi concat chain should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("[C]error: Invalid syntax"),
        "unexpected stub ansi concat chain output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn generic_type_bound_subroutine_dispatch_uses_matching_specific() {
    let src = write_program(
        "module m\n  implicit none\n  type :: datetime_t\n    integer :: year = 0\n    integer :: month = 0\n    integer :: day = 0\n  end type\n  type :: keyval_t\n    real :: as_float = -1.0\n    type(datetime_t) :: as_datetime\n  contains\n    procedure :: set_float\n    procedure :: set_datetime\n    procedure :: get_float\n    procedure :: get_datetime\n    generic :: set => set_float, set_datetime\n    generic :: get => get_float, get_datetime\n  end type\ncontains\n  subroutine set_float(self, x)\n    class(keyval_t), intent(inout) :: self\n    real, intent(in) :: x\n    self%as_float = x\n  end subroutine\n  subroutine set_datetime(self, x)\n    class(keyval_t), intent(inout) :: self\n    type(datetime_t), intent(in) :: x\n    self%as_datetime = x\n  end subroutine\n  subroutine get_float(self, x)\n    class(keyval_t), intent(in) :: self\n    real, intent(out) :: x\n    x = self%as_float\n  end subroutine\n  subroutine get_datetime(self, x)\n    class(keyval_t), intent(in) :: self\n    type(datetime_t), intent(out) :: x\n    x = self%as_datetime\n  end subroutine\nend module\nprogram p\n  use m\n  implicit none\n  type(keyval_t) :: kv\n  type(datetime_t) :: in_dt, out_dt\n  real :: out_float\n  in_dt = datetime_t(2022, 7, 31)\n  call kv%set(in_dt)\n  call kv%get(out_dt)\n  if (out_dt%year /= 2022) error stop 1\n  if (out_dt%month /= 7) error stop 2\n  if (out_dt%day /= 31) error stop 3\n  call kv%set(3.5)\n  call kv%get(out_float)\n  if (abs(out_float - 3.5) > 1.0e-6) error stop 4\n  print *, out_dt%year, out_dt%month, out_dt%day, out_float\nend program\n",
        "f90",
    );
    let out = unique_path("generic_type_bound_subroutine_dispatch", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("generic type-bound dispatch compile failed to spawn");
    assert!(
        compile.status.success(),
        "generic type-bound dispatch compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("generic type-bound dispatch run failed");
    assert!(
        run.status.success(),
        "generic type-bound dispatch run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    let fields: Vec<&str> = stdout.split_whitespace().collect();
    assert_eq!(&fields[..3], ["2022", "7", "31"]);
    let parsed_real: f32 = fields[3]
        .parse()
        .expect("expected final real field to parse as f32");
    assert!(
        (parsed_real - 3.5).abs() < 1.0e-6,
        "unexpected generic type-bound dispatch output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn generic_type_bound_dispatch_uses_character_intrinsic_actual_specific() {
    let src = write_program(
        "module m\n  implicit none\n  type, abstract :: generic_value\n  end type\n  type :: datetime_t\n    integer :: year = 0\n  end type\n  type, extends(generic_value) :: datetime_value\n    type(datetime_t) :: raw\n  end type\n  type, extends(generic_value) :: string_value\n    character(:), allocatable :: raw\n  end type\n  type :: keyval_t\n    class(generic_value), allocatable :: val\n  contains\n    procedure :: set_datetime\n    procedure :: set_string\n    procedure :: get_datetime\n    procedure :: get_string\n    procedure :: get_type\n    generic :: set => set_datetime, set_string\n    generic :: get => get_datetime, get_string\n  end type\ncontains\n  subroutine set_datetime(self, x)\n    class(keyval_t), intent(inout) :: self\n    type(datetime_t), intent(in) :: x\n    type(datetime_value), allocatable :: tmp\n    allocate(tmp)\n    tmp%raw = x\n    call move_alloc(tmp, self%val)\n  end subroutine\n\n  subroutine set_string(self, x)\n    class(keyval_t), intent(inout) :: self\n    character(*), intent(in) :: x\n    type(string_value), allocatable :: tmp\n    allocate(tmp)\n    tmp%raw = x\n    call move_alloc(tmp, self%val)\n  end subroutine\n\n  subroutine get_datetime(self, x)\n    class(keyval_t), intent(in) :: self\n    type(datetime_t), pointer, intent(out) :: x\n    nullify(x)\n    select type(v => self%val)\n    type is(datetime_value)\n      x => v%raw\n    end select\n  end subroutine\n\n  subroutine get_string(self, x)\n    class(keyval_t), intent(in) :: self\n    character(:), pointer, intent(out) :: x\n    nullify(x)\n    select type(v => self%val)\n    type is(string_value)\n      x => v%raw\n    end select\n  end subroutine\n\n  pure function get_type(self) result(code)\n    class(keyval_t), intent(in) :: self\n    integer :: code\n    select type(v => self%val)\n    class default\n      code = 0\n    type is(datetime_value)\n      code = 105\n    type is(string_value)\n      code = 101\n    end select\n  end function\nend module\nprogram p\n  use m\n  implicit none\n  type(keyval_t) :: kv\n  character(:), pointer :: sp\n  call kv%set(repeat('a', 3))\n  if (kv%get_type() /= 101) error stop 1\n  call kv%get(sp)\n  if (.not.associated(sp)) error stop 2\n  if (len(sp) /= 3) error stop 3\n  if (sp /= 'aaa') error stop 4\n  print *, kv%get_type(), len(sp), sp\nend program\n",
        "f90",
    );
    let out = unique_path("generic_type_bound_character_intrinsic_dispatch", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("generic type-bound character intrinsic dispatch compile failed to spawn");
    assert!(
        compile.status.success(),
        "generic type-bound character intrinsic dispatch compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("generic type-bound character intrinsic dispatch run failed");
    assert!(
        run.status.success(),
        "generic type-bound character intrinsic dispatch run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("101") && stdout.contains("3") && stdout.contains("aaa"),
        "unexpected generic type-bound character intrinsic dispatch output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn imported_derived_parameter_constructor_defaults_preserve_component_values() {
    let src = write_program(
        "module constants_m\n  implicit none\n  private\n  public :: enum_type, toml_type\n  type :: enum_type\n    integer :: invalid = 100\n    integer :: string = 101\n    integer :: boolean = 102\n  end type\n  type(enum_type), parameter :: toml_type = enum_type()\nend module\nmodule use_m\n  use constants_m, only : toml_type\n  implicit none\ncontains\n  integer function string_tag() result(v)\n    v = toml_type%string\n  end function\n  integer function invalid_tag() result(v)\n    v = toml_type%invalid\n  end function\nend module\nprogram p\n  use use_m\n  implicit none\n  if (string_tag() /= 101) error stop 1\n  if (invalid_tag() /= 100) error stop 2\n  print *, string_tag(), invalid_tag()\nend program\n",
        "f90",
    );
    let out = unique_path("imported_derived_parameter_ctor_defaults", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("imported derived parameter constructor defaults compile failed to spawn");
    assert!(
        compile.status.success(),
        "imported derived parameter constructor defaults should compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("imported derived parameter constructor defaults run failed");
    assert!(
        run.status.success(),
        "imported derived parameter constructor defaults should run: status={:?} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("101") && stdout.contains("100"),
        "unexpected imported derived parameter constructor defaults output: {}",
        stdout
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn select_type_branch_keeps_imported_derived_parameter_component_constants() {
    let dir = unique_dir("select_type_imported_derived_param");
    let constants_src = write_program_in(
        &dir,
        "constants_m.f90",
        "module constants_m\n  implicit none\n  private\n  public :: enum_type, tags\n  type :: enum_type\n    integer :: invalid = 100\n    integer :: string = 101\n  end type\n  type(enum_type), parameter :: tags = enum_type()\nend module\n",
    );
    let kv_src = write_program_in(
        &dir,
        "kv_m.f90",
        "module kv_m\n  use constants_m, only : tags\n  implicit none\n  private\n  public :: keyval, new_keyval\n  type, abstract :: generic_value\n  end type\n  type, extends(generic_value) :: string_value\n    character(:), allocatable :: raw\n  end type\n  type :: keyval\n    class(generic_value), allocatable :: val\n  contains\n    procedure :: set_string\n    procedure :: get_type\n  end type\ncontains\n  subroutine new_keyval(self)\n    type(keyval), intent(out) :: self\n  end subroutine\n  subroutine set_string(self, val)\n    class(keyval), intent(inout) :: self\n    character(*), intent(in) :: val\n    type(string_value), allocatable :: tmp\n    allocate(tmp)\n    tmp%raw = val\n    call move_alloc(tmp, self%val)\n  end subroutine\n  pure function get_type(self) result(value_type)\n    class(keyval), intent(in) :: self\n    integer :: value_type\n    select type(val => self%val)\n    class default\n      value_type = tags%invalid\n    type is(string_value)\n      value_type = tags%string\n    end select\n  end function\nend module\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use kv_m, only : keyval, new_keyval\n  implicit none\n  type(keyval) :: kv\n  call new_keyval(kv)\n  call kv%set_string('aaa')\n  if (kv%get_type() /= 101) error stop 1\n  print *, kv%get_type()\nend program\n",
    );

    let constants_obj = dir.join("constants_m.o");
    let kv_obj = dir.join("kv_m.o");
    let main_obj = dir.join("main.o");
    let exe = dir.join("select_type_imported_derived_param.bin");

    for (src, obj, needs_i) in [
        (&constants_src, &constants_obj, false),
        (&kv_src, &kv_obj, true),
        (&main_src, &main_obj, true),
    ] {
        let mut cmd = Command::new(compiler("armfortas"));
        cmd.current_dir(&dir).arg("-c");
        if needs_i {
            cmd.args(["-I", dir.to_str().unwrap()]);
        }
        cmd.args([
            "-J",
            dir.to_str().unwrap(),
            src.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ]);
        let compile = cmd.output().expect("compile spawn failed");
        assert!(
            compile.status.success(),
            "select type imported derived param compile failed for {}: {}",
            src.display(),
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let link = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            constants_obj.to_str().unwrap(),
            kv_obj.to_str().unwrap(),
            main_obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "select type imported derived param link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );

    let run = Command::new(&exe).output().expect("run failed");
    assert!(
        run.status.success(),
        "select type imported derived param run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("101"),
        "unexpected select type imported derived param output: {}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[test]
fn polymorphic_component_array_indexing_is_not_misread_as_tbp_dispatch() {
    let src = write_program(
        "module repro\n  implicit none\n  type :: token_t\n    integer :: x = 0\n  end type token_t\n  type :: base_t\n    integer :: pos = 0\n  contains\n    procedure :: noop\n  end type base_t\n  type, extends(base_t) :: child_t\n    type(token_t), allocatable :: token(:)\n  contains\n    procedure :: next\n  end type child_t\ncontains\n  subroutine noop(self)\n    class(base_t), intent(inout) :: self\n    self%pos = self%pos\n  end subroutine noop\n  subroutine next(self, token)\n    class(child_t), intent(inout) :: self\n    type(token_t), intent(out) :: token\n    self%pos = self%pos + 1\n    token = self%token(self%pos)\n  end subroutine next\nend module repro\nprogram p\n  use repro\n  implicit none\n  type(child_t) :: child\n  type(token_t) :: tok\n  allocate(child%token(1))\n  child%token(1)%x = 7\n  call child%next(tok)\n  if (tok%x /= 7) error stop 1\n  print *, 'ok'\nend program p\n",
        "f90",
    );
    let out = unique_path("polymorphic_component_array_indexing", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("polymorphic component array indexing compile failed to spawn");
    assert!(
        compile.status.success(),
        "polymorphic component array indexing compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&out)
        .output()
        .expect("polymorphic component array indexing run failed");
    assert!(
        run.status.success(),
        "polymorphic component array indexing run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected polymorphic component array indexing output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}
