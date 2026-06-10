//! l01: the F2023 million-character statement limit, exercised with a
//! generated source file (a checked-in ~1 MB fixture would make every
//! harness run at every opt level pay for it — sprint-doc pitfall).
//! Compile-only via `-S`, so this runs on every host. Acceptance never
//! changes: the over-limit statement still compiles; only the
//! conformance warning under --std=f2023 distinguishes it.

use std::path::{Path, PathBuf};
use std::process::Command;

fn compiler() -> PathBuf {
    for dir in ["target/debug", "../target/debug"] {
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
