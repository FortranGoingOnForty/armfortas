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
    let candidate = PathBuf::from("target/release").join(name);
    if candidate.exists() {
        return candidate;
    }
    let candidate = PathBuf::from("target/debug").join(name);
    assert!(
        candidate.exists(),
        "compiler binary '{}' not built — run `cargo build --bins` first",
        name
    );
    candidate
}

fn unique_path(stem: &str, ext: &str) -> PathBuf {
    let pid = std::process::id();
    let nanos = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap()
        .as_nanos();
    std::env::temp_dir().join(format!("afs_cli_{}_{}_{}.{}", stem, pid, nanos, ext))
}

fn write_program(text: &str, suffix: &str) -> PathBuf {
    let path = unique_path("src", suffix);
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
    assert!(out.stderr.is_empty(), "stderr should be empty: {:?}", String::from_utf8_lossy(&out.stderr));
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
    // Both binaries are built from the same source so the version
    // string is identical — that's the contract.
    assert!(stdout.contains("armfortas"));
}

#[test]
fn no_args_prints_help_to_stderr_and_exits_nonzero() {
    let out = Command::new(compiler("armfortas"))
        .output()
        .expect("failed to spawn armfortas");
    assert!(!out.status.success(), "no-arg invocation should fail");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        stderr.contains("USAGE"),
        "no-arg invocation should print help to stderr: {}",
        stderr
    );
}

#[test]
fn dash_c_produces_object_file_only() {
    let src = write_program(
        "module foo\n  integer :: x = 1\nend module\n",
        "f90",
    );
    let out = unique_path("obj", "o");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
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
fn dash_capital_s_produces_assembly_text() {
    let src = write_program(
        "program p\n  print *, 1\nend program\n",
        "f90",
    );
    let out = unique_path("asm", "s");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-S",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "-S compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let asm = std::fs::read_to_string(&out).expect("missing asm output");
    assert!(asm.contains("__TEXT"), ".s output should contain section directive");
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
        .args([
            "-E",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
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
fn std_f95_rejects_f2008_error_stop() {
    let src = write_program(
        "program p\n  error stop 'oops'\nend program\n",
        "f90",
    );
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
    assert!(!result.status.success(), "--std=f95 should reject ERROR STOP");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("ERROR STOP") && stderr.contains("F2008"),
        "expected ERROR STOP / F2008 error: {}",
        stderr
    );
    let _ = std::fs::remove_file(&src);
}

#[test]
fn response_file_supplies_arguments() {
    let src = write_program(
        "program p\n  print *, 7\nend program\n",
        "f90",
    );
    let out = unique_path("resp", "bin");
    let resp = unique_path("flags", "txt");
    std::fs::write(
        &resp,
        format!(
            "-O1\n-o\n{}\n{}\n",
            out.display(),
            src.display()
        ),
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
fn dash_j_writes_amod_to_chosen_directory() {
    let src = write_program(
        "module dashj_mod\n  integer :: y = 5\nend module\n",
        "f90",
    );
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
fn verbose_flag_streams_phase_lines_to_stderr() {
    let src = write_program(
        "program p\n  print *, 1\nend program\n",
        "f90",
    );
    let out = unique_path("verbose", "bin");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-v",
            src.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("spawn failed");
    assert!(result.status.success());
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(stderr.contains("preprocessing:"), "verbose missing preprocessing line: {}", stderr);
    assert!(stderr.contains("codegen:"), "verbose missing codegen line: {}", stderr);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn time_report_prints_phase_table() {
    let src = write_program(
        "program p\n  print *, 1\nend program\n",
        "f90",
    );
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
    assert!(stderr.contains("Phase"), "missing time-report header: {}", stderr);
    assert!(stderr.contains("Total"), "missing time-report total: {}", stderr);
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}

#[test]
fn diagnostic_renders_source_line_and_caret() {
    let src = write_program(
        "program p\n  error stop 'oops'\nend program\n",
        "f90",
    );
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
    assert!(stderr.contains(":2:3: error:"), "missing standard error header: {}", stderr);
    // Source line is shown with a numbered gutter (`    2 |`).
    assert!(stderr.contains("|   error stop"), "missing source-line snippet: {}", stderr);
    // Caret underline lives on a `      |` line.
    assert!(stderr.contains("      |"), "missing caret gutter: {}", stderr);
    assert!(stderr.contains("^"), "missing caret marker: {}", stderr);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn no_color_env_suppresses_ansi_escapes() {
    let src = write_program(
        "program p\n  error stop 'x'\nend program\n",
        "f90",
    );
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
    assert!(!stderr.contains('\x1b'), "NO_COLOR must suppress ANSI escapes: {:?}", stderr);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn clicolor_force_enables_ansi_even_off_a_tty() {
    let src = write_program(
        "program p\n  error stop 'x'\nend program\n",
        "f90",
    );
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
    assert!(stderr.contains('\x1b'), "CLICOLOR_FORCE must produce ANSI escapes: {:?}", stderr);
    let _ = std::fs::remove_file(&src);
    let _ = std::fs::remove_file(&out);
}

#[test]
fn fimplicit_none_rejects_implicitly_typed_use() {
    let src = write_program(
        "program p\n  i = 5\n  print *, i\nend program\n",
        "f90",
    );
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
    assert!(!result.status.success(), "-fimplicit-none should reject undeclared 'i'");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("'i'") && stderr.contains("IMPLICIT NONE is active"),
        "expected implicit-none diagnostic: {}",
        stderr
    );
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
    assert!(result.status.success(), "-fdefault-integer-8 compile failed: {}",
        String::from_utf8_lossy(&result.stderr));
    let run = Command::new(&out).output().expect("run failed");
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(stdout.trim().ends_with('8'), "expected kind 8: {:?}", stdout);
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
    assert!(stdout.trim().ends_with('8'), "expected kind 8: {:?}", stdout);
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
