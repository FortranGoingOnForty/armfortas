//! End-to-end test harness for ARMFORTAS.
//!
//! Discovers `.f90` files in `test_programs/`, compiles each with `armfortas`,
//! runs the binary, and checks stdout against `! CHECK:` annotations.
//!
//! Each `! CHECK:` line specifies a substring that must appear in the output,
//! in order. Whitespace is trimmed for comparison.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A single expected check.
struct Check {
    line_num: usize,
    pattern: String,
}

/// Extract `! CHECK:` patterns from a Fortran source file.
fn extract_checks(source: &str) -> Vec<Check> {
    source
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("! CHECK:") {
                Some(Check {
                    line_num: i + 1,
                    pattern: rest.trim().to_string(),
                })
            } else {
                None
            }
        })
        .collect()
}

/// Match checks against actual output lines. Checks must appear in order
/// but not necessarily consecutively — intervening output lines are allowed.
fn match_checks(checks: &[Check], output: &str, filename: &str) -> Result<(), String> {
    let output_lines: Vec<&str> = output.lines().collect();
    let mut output_idx = 0;

    for check in checks {
        let mut found = false;
        while output_idx < output_lines.len() {
            if output_lines[output_idx].trim().contains(&check.pattern) {
                found = true;
                output_idx += 1;
                break;
            }
            output_idx += 1;
        }
        if !found {
            return Err(format!(
                "{}:{}: CHECK failed: expected '{}' not found in remaining output\n\
                 Full output:\n{}",
                filename, check.line_num, check.pattern, output
            ));
        }
    }

    Ok(())
}

/// Find the armfortas binary.
fn find_compiler() -> PathBuf {
    // Look in cargo's target directory.
    let candidates = [
        "target/debug/armfortas",
        "target/release/armfortas",
    ];
    for c in &candidates {
        let p = PathBuf::from(c);
        if p.exists() {
            return p;
        }
    }
    panic!("cannot find armfortas binary — run `cargo build` first");
}

/// Find the test_programs directory.
fn find_test_programs() -> PathBuf {
    let candidates = [
        "test_programs",
        "../test_programs",
    ];
    for c in &candidates {
        let p = PathBuf::from(c);
        if p.is_dir() {
            return p;
        }
    }
    panic!("cannot find test_programs/ directory");
}

/// Run a single test program: compile, execute, check output.
fn run_test(compiler: &Path, source: &Path) -> Result<(), String> {
    let filename = source.file_name().unwrap().to_str().unwrap();
    let source_text = fs::read_to_string(source)
        .map_err(|e| format!("{}: cannot read: {}", filename, e))?;

    let checks = extract_checks(&source_text);
    if checks.is_empty() {
        return Err(format!("{}: no CHECK annotations found", filename));
    }

    // Compile.
    let binary = std::env::temp_dir().join(format!(
        "afs_test_{}",
        source.file_stem().unwrap().to_str().unwrap()
    ));
    let compile = Command::new(compiler)
        .args([
            source.to_str().unwrap(),
            "-o",
            binary.to_str().unwrap(),
        ])
        .output()
        .map_err(|e| format!("{}: cannot run compiler: {}", filename, e))?;

    if !compile.status.success() {
        let stderr = String::from_utf8_lossy(&compile.stderr);
        return Err(format!("{}: compilation failed:\n{}", filename, stderr));
    }

    // Execute.
    let run = Command::new(&binary)
        .output()
        .map_err(|e| format!("{}: cannot run binary: {}", filename, e))?;

    if !run.status.success() {
        let stderr = String::from_utf8_lossy(&run.stderr);
        return Err(format!(
            "{}: execution failed (exit {}): {}",
            filename,
            run.status.code().unwrap_or(-1),
            stderr
        ));
    }

    let stdout = String::from_utf8_lossy(&run.stdout);

    // Check output.
    match_checks(&checks, &stdout, filename)?;

    // Cleanup.
    let _ = fs::remove_file(&binary);

    Ok(())
}

#[test]
fn test_programs_end_to_end() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();

    let mut sources: Vec<PathBuf> = fs::read_dir(&test_dir)
        .expect("cannot read test_programs/")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "f90").unwrap_or(false))
        .collect();
    sources.sort();

    assert!(!sources.is_empty(), "no .f90 files found in test_programs/");

    let mut failures = Vec::new();
    let mut passed = 0;

    for source in &sources {
        match run_test(&compiler, source) {
            Ok(()) => {
                passed += 1;
                eprintln!("  PASS: {}", source.file_name().unwrap().to_str().unwrap());
            }
            Err(msg) => {
                eprintln!("  FAIL: {}", source.file_name().unwrap().to_str().unwrap());
                failures.push(msg);
            }
        }
    }

    eprintln!("\n{} passed, {} failed out of {} test programs",
        passed, failures.len(), sources.len());

    if !failures.is_empty() {
        let msg = failures.join("\n\n");
        panic!("Test failures:\n\n{}", msg);
    }
}
