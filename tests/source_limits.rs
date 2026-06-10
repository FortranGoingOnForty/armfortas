//! l01: the F2023 million-character statement limit, exercised with a
//! generated source file (a checked-in ~1 MB fixture would make every
//! harness run at every opt level pay for it — sprint-doc pitfall).
//! Compile-only via `-S`, so this runs on every host. Acceptance never
//! changes: the over-limit statement still compiles; only the
//! conformance warning under --std=f2023 distinguishes it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn compiler() -> PathBuf {
    // CARGO_BIN_EXE_armfortas points at the profile-correct binary
    // (release on CI); the path probes are the fallback for direct
    // `cargo test --test source_limits` runs.
    if let Some(path) = std::env::var_os("CARGO_BIN_EXE_armfortas") {
        return PathBuf::from(path);
    }
    for dir in [
        "target/debug",
        "../target/debug",
        "target/release",
        "../target/release",
    ] {
        let p = Path::new(dir).join("armfortas");
        if p.exists() {
            return p;
        }
    }
    panic!("armfortas binary not built — run cargo build first");
}

/// One statement totalling just over a million characters, spread over
/// ~130-char continuation lines (under 132, so the f2018 control run
/// is line-warning-clean).
fn million_char_program() -> String {
    let mut src = String::with_capacity(1_100_000);
    src.push_str("program big_stmt\n  implicit none\n  integer :: total\n");
    src.push_str("  total = 0 &\n");
    let term = format!("    + {} &\n", "0".repeat(120));
    // The limit counts the statement's source characters; line content
    // is term minus the newline. Overshoot by a margin so the total is
    // unambiguously past one million.
    let lines_needed = 1_000_000 / (term.len() - 1) + 100;
    for _ in 0..lines_needed {
        src.push_str(&term);
    }
    src.push_str("    + 1\n  print *, total\nend program big_stmt\n");
    src
}

fn compile_s(src_path: &Path, out: &Path, std_flag: &str) -> std::process::Output {
    Command::new(compiler())
        .args([std_flag, "-S"])
        .arg(src_path)
        .arg("-o")
        .arg(out)
        .output()
        .expect("cannot run armfortas")
}

#[test]
fn million_char_statement_compiles_and_warns_only_under_f2023() {
    let dir = std::env::temp_dir().join(format!("afs_srclim_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f90 = dir.join("big_stmt.f90");
    std::fs::write(&f90, million_char_program()).unwrap();
    let asm = dir.join("big_stmt.s");

    let r = compile_s(&f90, &asm, "--std=f2023");
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(
        r.status.success(),
        "over-limit statement must still compile (warning, never error):\n{}",
        stderr
    );
    assert!(
        stderr.contains("statement is") && stderr.contains("1,000,000"),
        "expected the F2023 statement-length conformance warning, got:\n{}",
        stderr
    );

    let r = compile_s(&f90, &asm, "--std=f2018");
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(r.status.success(), "f2018 compile failed:\n{}", stderr);
    assert!(
        !stderr.contains("statement is"),
        "statement-length warning is F2023-only, got:\n{}",
        stderr
    );
    assert!(
        !stderr.contains("warning"),
        "control run should be warning-free (lines are under 132):\n{}",
        stderr
    );

    let _ = std::fs::remove_dir_all(&dir);
}

/// The explosion boundary (l01 follow-up): a maximal-depth chain
/// inside the statement cap must compile — no stack fault at any
/// depth the cap admits. Minimal 3-char continuation lines give the
/// deepest tree per character.
#[test]
fn deep_chain_within_cap_compiles() {
    let n = 600_000;
    let mut src = String::with_capacity(4 * n);
    src.push_str("program p\nimplicit none\ninteger :: total\ntotal=0&\n");
    for _ in 0..n - 1 {
        src.push_str("+1&\n");
    }
    src.push_str("+1\nprint *, total\nend program p\n");
    let dir = std::env::temp_dir().join(format!("afs_deepchain_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f90 = dir.join("deep.f90");
    std::fs::write(&f90, src).unwrap();
    let r = compile_s(&f90, &dir.join("deep.s"), "--std=f2023");
    assert!(
        r.status.success(),
        "a {}-term chain inside the cap must compile (status {:?}):\n{}",
        n,
        r.status,
        String::from_utf8_lossy(&r.stderr)
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Past the cap: a clean diagnostic and exit 1 — never a stack fault.
#[test]
fn over_cap_statement_errors_cleanly() {
    let n = 800_000;
    let mut src = String::with_capacity(8 * n);
    src.push_str("program p\nimplicit none\ninteger :: total\ntotal=0&\n");
    for _ in 0..n {
        src.push_str("+1     &\n"); // fat lines: past 2M chars
    }
    src.push_str("+1\nprint *, total\nend program p\n");
    let dir = std::env::temp_dir().join(format!("afs_overcap_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f90 = dir.join("overcap.f90");
    std::fs::write(&f90, src).unwrap();
    let r = compile_s(&f90, &dir.join("overcap.s"), "--std=f2023");
    assert_eq!(
        r.status.code(),
        Some(1),
        "over-cap statement must exit 1 (a None code means a signal — the stack fault this gate exists to prevent)"
    );
    let stderr = String::from_utf8_lossy(&r.stderr);
    assert!(
        stderr.contains("compiler limit"),
        "expected the statement-cap diagnostic, got:\n{}",
        stderr
    );
    let _ = std::fs::remove_dir_all(&dir);
}

/// Pathological paren nesting stops at the parser's nesting limit with
/// a clean error (pre-existing guard, locked here alongside the cap).
#[test]
fn deep_paren_nesting_errors_cleanly() {
    let depth = 5_000;
    let src = format!(
        "program p\nimplicit none\ninteger :: x\nx = {}1{}\nprint *, x\nend program p\n",
        "(".repeat(depth),
        ")".repeat(depth)
    );
    let dir = std::env::temp_dir().join(format!("afs_deepparen_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let f90 = dir.join("paren.f90");
    std::fs::write(&f90, src).unwrap();
    let r = compile_s(&f90, &dir.join("paren.s"), "--std=f2023");
    assert_eq!(
        r.status.code(),
        Some(1),
        "deep nesting must exit 1, not fault"
    );
    assert!(
        String::from_utf8_lossy(&r.stderr).contains("nesting exceeds parser limit"),
        "expected the parser nesting diagnostic"
    );
    let _ = std::fs::remove_dir_all(&dir);
}
