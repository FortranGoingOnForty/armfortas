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

/// Run a single test program: compile at the given optimization level,
/// execute, check output.
fn run_test(compiler: &Path, source: &Path, opt_flag: &str) -> Result<(), String> {
    let filename = source.file_name().unwrap().to_str().unwrap();
    let source_text = fs::read_to_string(source)
        .map_err(|e| format!("{}: cannot read: {}", filename, e))?;

    let checks = extract_checks(&source_text);
    if checks.is_empty() {
        return Err(format!("{}: no CHECK annotations found", filename));
    }

    // Compile. Use a per-(file,level) binary path so concurrent jobs
    // and successive runs at different levels don't stomp each other.
    let stem = source.file_stem().unwrap().to_str().unwrap();
    let level = opt_flag.trim_start_matches('-');
    let binary = std::env::temp_dir().join(format!("afs_test_{}_{}", stem, level));
    let compile = Command::new(compiler)
        .args([
            source.to_str().unwrap(),
            opt_flag,
            "-o",
            binary.to_str().unwrap(),
        ])
        .output()
        .map_err(|e| format!("{}: cannot run compiler: {}", filename, e))?;

    if !compile.status.success() {
        let stderr = String::from_utf8_lossy(&compile.stderr);
        return Err(format!("{} [{}]: compilation failed:\n{}", filename, opt_flag, stderr));
    }

    // Per-(file,level) sandbox directory. Test programs that touch the
    // filesystem (open(file=...)) write into this directory via relative
    // paths, which keeps the three parallel test_programs_end_to_end_o*
    // threads from racing on shared paths.
    let sandbox = std::env::temp_dir().join(format!("afs_test_sandbox_{}_{}", stem, level));
    let _ = fs::remove_dir_all(&sandbox);
    fs::create_dir_all(&sandbox)
        .map_err(|e| format!("{}: cannot create sandbox dir {}: {}", filename, sandbox.display(), e))?;

    // Execute.
    let run = Command::new(&binary)
        .current_dir(&sandbox)
        .output()
        .map_err(|e| format!("{}: cannot run binary: {}", filename, e))?;

    if !run.status.success() {
        let stderr = String::from_utf8_lossy(&run.stderr);
        return Err(format!(
            "{} [{}]: execution failed (exit {}): {}",
            filename,
            opt_flag,
            run.status.code().unwrap_or(-1),
            stderr
        ));
    }

    let stdout = String::from_utf8_lossy(&run.stdout);

    // Check output. Same CHECK annotations are enforced at every opt
    // level — this is the correctness invariant.
    let label = format!("{} [{}]", filename, opt_flag);
    match_checks(&checks, &stdout, &label)?;

    // Cleanup.
    let _ = fs::remove_file(&binary);
    let _ = fs::remove_dir_all(&sandbox);

    Ok(())
}

/// Discover the test programs and run each at every supported opt level.
/// This enforces the correctness invariant: same source must produce
/// the same output regardless of optimization level.
fn run_all_at(opt_flag: &str) -> Result<(), String> {
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
        match run_test(&compiler, source, opt_flag) {
            Ok(()) => {
                passed += 1;
                eprintln!("  PASS [{}]: {}", opt_flag,
                    source.file_name().unwrap().to_str().unwrap());
            }
            Err(msg) => {
                eprintln!("  FAIL [{}]: {}", opt_flag,
                    source.file_name().unwrap().to_str().unwrap());
                failures.push(msg);
            }
        }
    }

    eprintln!("\n[{}] {} passed, {} failed out of {} test programs",
        opt_flag, passed, failures.len(), sources.len());

    if failures.is_empty() {
        Ok(())
    } else {
        Err(failures.join("\n\n"))
    }
}

#[test]
fn test_programs_end_to_end() {
    if let Err(msg) = run_all_at("-O0") {
        panic!("Test failures at -O0:\n\n{}", msg);
    }
}

#[test]
fn test_programs_end_to_end_o1() {
    if let Err(msg) = run_all_at("-O1") {
        panic!("Test failures at -O1:\n\n{}", msg);
    }
}

#[test]
fn test_programs_end_to_end_o2() {
    if let Err(msg) = run_all_at("-O2") {
        panic!("Test failures at -O2:\n\n{}", msg);
    }
}

#[test]
fn test_programs_end_to_end_o3() {
    if let Err(msg) = run_all_at("-O3") {
        panic!("Test failures at -O3:\n\n{}", msg);
    }
}

#[test]
fn test_programs_end_to_end_ofast() {
    if let Err(msg) = run_all_at("-Ofast") {
        panic!("Test failures at -Ofast:\n\n{}", msg);
    }
}

/// Determinism regression: compile a program twice at -O2 and
/// require byte-identical machine code. Codegen non-determinism
/// (HashMap iteration order, stale spill-victim entries, sort
/// tie-breaking) caused this to flake during the mem2reg work; the
/// test pins the invariant going forward so any future regression
/// trips immediately instead of intermittently.
fn compile_to_asm(compiler: &Path, source: &Path, opt: &str) -> Vec<u8> {
    let asm_path = std::env::temp_dir().join(format!(
        "afs_det_{}_{}_{}.s",
        std::process::id(),
        source.file_stem().unwrap().to_str().unwrap(),
        opt.trim_start_matches('-'),
    ));
    let status = Command::new(compiler)
        .args([
            source.to_str().unwrap(),
            opt,
            "-S",
            "-o",
            asm_path.to_str().unwrap(),
        ])
        .status()
        .expect("compiler launch failed");
    assert!(status.success(), "-S compile failed");
    let bytes = fs::read(&asm_path).expect("cannot read emitted .s");
    let _ = fs::remove_file(&asm_path);
    bytes
}

#[test]
fn codegen_is_deterministic_at_o2() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("two_loops.f90");
    assert!(source.exists(), "two_loops.f90 missing — needed for determinism check");

    let first = compile_to_asm(&compiler, &source, "-O2");
    let second = compile_to_asm(&compiler, &source, "-O2");
    assert_eq!(
        first, second,
        "two compilations of the same source produced different assembly — \
         determinism regression. This usually means a HashMap iteration \
         order leak in codegen."
    );
}

/// Determinism regression for programs that import module globals.
/// Audit B-3: `install_globals_as_locals` iterated a HashMap, so
/// the emitted `global_addr` instructions landed in non-deterministic
/// positions — liveness and regalloc then produced different .s
/// output. This test pins the fix for every opt level that runs a
/// register allocator.
#[test]
fn codegen_is_deterministic_with_module_globals() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("module_init.f90");
    assert!(source.exists(), "module_init.f90 missing — needed for determinism check");

    for opt in ["-O0", "-O1", "-O2", "-O3"] {
        let first = compile_to_asm(&compiler, &source, opt);
        let second = compile_to_asm(&compiler, &source, opt);
        assert_eq!(
            first, second,
            "two compilations of module_init.f90 produced different assembly at {} — \
             this usually means install_globals_as_locals is iterating a HashMap \
             in non-deterministic order.",
            opt,
        );
    }
}
