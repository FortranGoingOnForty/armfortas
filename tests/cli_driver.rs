//! Sprint 32 CLI driver tests.
//!
//! Each test exercises one user-visible behaviour of the `armfortas`
//! / `afs` driver via subprocess invocation.  Subprocess use is
//! deliberate — we want to catch wrong-exit-code, wrong-stdout-vs-
//! stderr-routing, and missing-symbol-from-bin issues that an
//! in-process API call wouldn't see.

use std::path::PathBuf;
use std::process::Command;

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
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("afs_cli_{}_{}_{}.{}", stem, pid, nanos, ext))
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
    assert!(run.status.success(), "fixed-form run failed: {:?}", run.status);
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
    assert!(out.exists(), "mixed-width select should produce an object file");

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
    assert!(out.exists(), "mixed-width max should produce an object file");

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
    assert!(
        !result.status.success(),
        "--std=f95 should reject IMPURE"
    );
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
    assert!(
        !result.status.success(),
        "--std=f77 should reject MODULE"
    );
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
        !stderr.contains("-Wpedantic is recognized but warning-group emission is not yet implemented"),
        "pedantic should now be a real semantic warning group: {}",
        stderr
    );
    assert!(
        !stderr.contains("-Wdeprecated is recognized but warning-group emission is not yet implemented"),
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
    let src = write_program("program p\n  integer :: x\n  common /blk/ x\nend program\n", "f90");
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
