//! End-to-end test harness for ARMFORTAS.
//!
//! Discovers `.f90` files in `test_programs/`, compiles each with `armfortas`,
//! runs the binary, and checks stdout against `! CHECK:` annotations.
//!
//! Each `! CHECK:` line specifies a substring that must appear in the output,
//! in order. Whitespace is trimmed for comparison.
//!
//! ## XFAIL annotations
//!
//! A program may carry one or more `! XFAIL: <reason>` lines anywhere in
//! the source. An XFAIL'd program is *expected* to fail at compile or
//! runtime (or to mismatch its CHECKs) — it's tracking a known bug. The
//! harness reports:
//!
//!   * `XFAIL`  — the program failed as expected. Counted as a pass.
//!   * `XPASS`  — the program unexpectedly succeeded. Counted as a
//!     failure so we get loud notification that the bug is now fixed
//!     and the XFAIL annotation should be removed.
//!
//! XFAIL'd programs are how we capture audit findings as living
//! regression tests *before* the underlying bug is fixed. Each finding
//! gets a program in `test_programs/` whose annotation references the
//! audit ID (`! XFAIL: audit BLOCKING-1 (implied-do negative step)`),
//! so a future audit can grep `test_programs/` for the finding ID and
//! immediately see whether the bug class is covered.
//!
//! ## ERROR_EXPECTED annotations
//!
//! A program may also carry an `! ERROR_EXPECTED: <substring>` line.
//! That asserts the program **must fail to compile**, and the
//! compiler's stderr **must contain** the given substring. This is
//! how we test "should be a diagnostic" cases — Fortran constraint
//! violations that the compiler is required to reject. The semantics:
//!
//!   * If `ERROR_EXPECTED` is present, CHECK lines are ignored.
//!   * If compilation succeeds, the test fails.
//!   * If compilation fails but stderr doesn't contain the substring,
//!     the test fails.
//!   * If compilation fails with the expected substring, the test
//!     passes.
//!
//! `ERROR_EXPECTED` composes with `XFAIL`: if a program is annotated
//! with both, an XFAIL fires when the expected error is **not**
//! produced (the bug is "we don't yet diagnose this"), and an XPASS
//! fires once we start diagnosing it correctly (so the XFAIL
//! annotation can come off and the program becomes a regular
//! "diagnostic regression" test).
//!
//! ## IR_CHECK / IR_NOT annotations
//!
//! For tests that need to assert on the *shape* of the lowered IR
//! (not just the runtime answer), two extra annotations exist:
//!
//!   * `! IR_CHECK: <substring>` — the substring must appear in the
//!     compiler's `--emit-ir` output. Multiple IR_CHECKs must appear
//!     in the order they're declared.
//!   * `! IR_NOT: <substring>` — the substring must NOT appear in the
//!     `--emit-ir` output. Used for negative-shape assertions like
//!     "this PARAMETER local must not have a `store` instruction"
//!     or "this expression must not lower to a `global_addr`".
//!
//! IR shape is only stable at -O0 (the optimization passes erase
//! dead code, fold constants, hoist loads, etc.), so IR_CHECK /
//! IR_NOT only fire at the -O0 test level. The runtime CHECKs
//! continue to run at every opt level as before.
//!
//! Audit5 MIN-2: this exists because audit4 captured the
//! parameter-inlining and module-allocatable bugs as runtime tests
//! only. A future regression that broke the IR shape but happened
//! to land on the right runtime answer would slip through.
//!
//! ## STDERR_CHECK / EXIT_CODE annotations
//!
//! Runtime tests can also assert on stderr and process exit status:
//!
//!   * `! STDERR_CHECK: <substring>` — ordered substring checks
//!     against the program's stderr stream.
//!   * `! EXIT_CODE: <int>` — exact process exit code. Without this
//!     annotation, the harness preserves the old rule that runtime
//!     tests must exit successfully.
//!
//! This makes runtime tests expressive enough for paths like
//! `ERROR STOP`, warning-like stderr output, and future
//! side-effect-heavy programs without forcing them through
//! `ERROR_EXPECTED`, which is compile-failure-only.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

/// A single expected check.
struct Check {
    line_num: usize,
    pattern: String,
}

/// Extract ordered substring checks from a Fortran source file.
fn extract_prefixed_checks(source: &str, prefix: &str) -> Vec<Check> {
    source
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix(prefix) {
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

/// Extract `! CHECK:` patterns from a Fortran source file.
fn extract_checks(source: &str) -> Vec<Check> {
    extract_prefixed_checks(source, "! CHECK:")
}

/// Extract `! STDERR_CHECK:` patterns from a Fortran source file.
fn extract_stderr_checks(source: &str) -> Vec<Check> {
    extract_prefixed_checks(source, "! STDERR_CHECK:")
}

/// Extract `! XFAIL:` reason text. Returns the first reason found, or
/// `None` if the program has no XFAIL annotation. Multiple XFAIL lines
/// are allowed (only the first is reported); a typical pattern is one
/// audit ID per line for findings of the same class.
fn extract_xfail(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("! XFAIL:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Extract `! ERROR_EXPECTED:` substring text. Returns the expected
/// stderr substring if any. Programs with this annotation are
/// asserted to fail compilation.
fn extract_error_expected(source: &str) -> Option<String> {
    for line in source.lines() {
        let trimmed = line.trim();
        if let Some(rest) = trimmed.strip_prefix("! ERROR_EXPECTED:") {
            return Some(rest.trim().to_string());
        }
    }
    None
}

/// Extract `! EXIT_CODE:` and parse it as an exact expected exit
/// status. Multiple annotations are rejected as a test setup error
/// so the expected runtime contract stays unambiguous.
fn extract_exit_code(source: &str, filename: &str) -> Result<Option<i32>, String> {
    let mut matches = source.lines().enumerate().filter_map(|(i, line)| {
        let trimmed = line.trim();
        trimmed
            .strip_prefix("! EXIT_CODE:")
            .map(|rest| (i + 1, rest.trim()))
    });

    let Some((line_num, raw)) = matches.next() else {
        return Ok(None);
    };

    if let Some((extra_line, _)) = matches.next() {
        return Err(format!(
            "{}:{}: multiple EXIT_CODE annotations are not allowed (another at line {})",
            filename, line_num, extra_line
        ));
    }

    raw.parse::<i32>().map(Some).map_err(|_| {
        format!(
            "{}:{}: EXIT_CODE must be a decimal integer, got '{}'",
            filename, line_num, raw
        )
    })
}

/// A single IR-shape assertion. Positive checks must appear in
/// order; negative checks must not appear at all. Source line
/// numbers are kept so failure messages can point at the right
/// annotation.
struct IrCheck {
    line_num: usize,
    pattern: String,
    negative: bool,
}

/// Extract `! IR_CHECK:` and `! IR_NOT:` annotations from a source.
fn extract_ir_checks(source: &str) -> Vec<IrCheck> {
    source
        .lines()
        .enumerate()
        .filter_map(|(i, line)| {
            let trimmed = line.trim();
            if let Some(rest) = trimmed.strip_prefix("! IR_CHECK:") {
                Some(IrCheck {
                    line_num: i + 1,
                    pattern: rest.trim().to_string(),
                    negative: false,
                })
            } else if let Some(rest) = trimmed.strip_prefix("! IR_NOT:") {
                Some(IrCheck {
                    line_num: i + 1,
                    pattern: rest.trim().to_string(),
                    negative: true,
                })
            } else {
                None
            }
        })
        .collect()
}

/// Apply IR shape assertions against an --emit-ir text dump.
/// Positive assertions match in declared order (intervening lines
/// are allowed). Negative assertions match against the entire
/// text — if the substring appears anywhere, the test fails.
fn match_ir_checks(checks: &[IrCheck], ir: &str, filename: &str) -> Result<(), String> {
    let mut search_offset = 0;
    for check in checks {
        if check.negative {
            if ir.contains(&check.pattern) {
                return Err(format!(
                    "{}:{}: IR_NOT failed: substring '{}' appears in IR\n\
                     Full IR:\n{}",
                    filename, check.line_num, check.pattern, ir,
                ));
            }
        } else {
            // Positive: search forward from the previous match
            // position so multiple IR_CHECKs enforce ordering.
            if let Some(rel) = ir[search_offset..].find(&check.pattern) {
                search_offset += rel + check.pattern.len();
            } else {
                return Err(format!(
                    "{}:{}: IR_CHECK failed: substring '{}' not found from offset {}\n\
                     Full IR:\n{}",
                    filename, check.line_num, check.pattern, search_offset, ir,
                ));
            }
        }
    }
    Ok(())
}

/// Match checks against actual output lines. Checks must appear in order
/// but not necessarily consecutively — intervening output lines are allowed.
fn match_checks(
    checks: &[Check],
    output: &str,
    filename: &str,
    directive_name: &str,
) -> Result<(), String> {
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
                "{}:{}: {} failed: expected '{}' not found in remaining output\n\
                 Full output:\n{}",
                filename, check.line_num, directive_name, check.pattern, output
            ));
        }
    }

    Ok(())
}

/// Find the armfortas binary.
fn find_compiler() -> PathBuf {
    // Look in cargo's target directory.
    let candidates = ["target/debug/armfortas", "target/release/armfortas"];
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
    let candidates = ["test_programs", "../test_programs"];
    for c in &candidates {
        let p = PathBuf::from(c);
        if p.is_dir() {
            return p;
        }
    }
    panic!("cannot find test_programs/ directory");
}

/// What happened when we ran a test program.
#[derive(Debug)]
enum TestOutcome {
    /// Compiled, ran, all CHECKs matched. No XFAIL annotation present.
    Pass,
    /// Marked XFAIL and failed somewhere — this is the expected
    /// outcome for an open audit finding. The reason is the XFAIL
    /// annotation text plus the underlying failure detail.
    Xfail(String),
    /// Marked XFAIL but unexpectedly succeeded. Loud failure: the
    /// underlying bug is fixed and the XFAIL annotation should be
    /// removed so the program becomes a regular regression test.
    Xpass(String),
    /// No XFAIL annotation, and the program failed somewhere.
    Fail(String),
}

/// Run a single test program: compile at the given optimization level,
/// execute, check output. Honors `! XFAIL:` annotations.
fn run_test(compiler: &Path, source: &Path, opt_flag: &str) -> TestOutcome {
    let filename = source.file_name().unwrap().to_str().unwrap();
    let source_text = match fs::read_to_string(source) {
        Ok(s) => s,
        Err(e) => return TestOutcome::Fail(format!("{}: cannot read: {}", filename, e)),
    };

    let xfail_reason = extract_xfail(&source_text);
    let error_expected = extract_error_expected(&source_text);
    let checks = extract_checks(&source_text);
    let stderr_checks = extract_stderr_checks(&source_text);
    let expected_exit_code = match extract_exit_code(&source_text, filename) {
        Ok(code) => code,
        Err(e) => return TestOutcome::Fail(e),
    };
    let ir_checks = extract_ir_checks(&source_text);
    if checks.is_empty()
        && stderr_checks.is_empty()
        && ir_checks.is_empty()
        && expected_exit_code.is_none()
        && xfail_reason.is_none()
        && error_expected.is_none()
    {
        // Programs with no runtime or shape assertions, no XFAIL marker,
        // and no ERROR marker are mis-configured tests, not test failures.
        return TestOutcome::Fail(format!(
            "{}: no CHECK / STDERR_CHECK / EXIT_CODE / IR_CHECK / XFAIL / ERROR_EXPECTED annotations",
            filename,
        ));
    }

    // Try the compile/run/check pipeline. Any failure path returns
    // an Err with a message; success returns Ok.
    let inner = || -> Result<(), String> {
        // Use a per-(file,level) binary path so concurrent jobs
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

        // ERROR_EXPECTED branch: compilation MUST fail with the
        // expected stderr substring. CHECKs are ignored.
        if let Some(expected) = &error_expected {
            if compile.status.success() {
                let _ = fs::remove_file(&binary);
                return Err(format!(
                    "{} [{}]: ERROR_EXPECTED({}) but compilation succeeded",
                    filename, opt_flag, expected,
                ));
            }
            let stderr = String::from_utf8_lossy(&compile.stderr);
            if !stderr.contains(expected.as_str()) {
                return Err(format!(
                    "{} [{}]: ERROR_EXPECTED({}) but stderr did not contain it.\n\
                     Full stderr:\n{}",
                    filename, opt_flag, expected, stderr,
                ));
            }
            return Ok(());
        }

        if !compile.status.success() {
            let stderr = String::from_utf8_lossy(&compile.stderr);
            return Err(format!(
                "{} [{}]: compilation failed:\n{}",
                filename, opt_flag, stderr,
            ));
        }

        // Per-(file,level) sandbox directory. Test programs that touch the
        // filesystem (open(file=...)) write into this directory via relative
        // paths, which keeps the parallel test_programs_end_to_end_o*
        // threads from racing on shared paths.
        let sandbox = std::env::temp_dir().join(format!("afs_test_sandbox_{}_{}", stem, level));
        let _ = fs::remove_dir_all(&sandbox);
        fs::create_dir_all(&sandbox).map_err(|e| {
            format!(
                "{}: cannot create sandbox dir {}: {}",
                filename,
                sandbox.display(),
                e
            )
        })?;

        let run = Command::new(&binary)
            .current_dir(&sandbox)
            .output()
            .map_err(|e| format!("{}: cannot run binary: {}", filename, e))?;

        let actual_exit_code = run.status.code().unwrap_or(-1);
        let stderr = String::from_utf8_lossy(&run.stderr);
        let expected_exit_code = expected_exit_code.unwrap_or(0);
        if actual_exit_code != expected_exit_code {
            let _ = fs::remove_file(&binary);
            let _ = fs::remove_dir_all(&sandbox);
            return Err(format!(
                "{} [{}]: execution exit mismatch: expected {}, got {}\n\
                 stderr:\n{}",
                filename, opt_flag, expected_exit_code, actual_exit_code, stderr,
            ));
        }

        let stdout = String::from_utf8_lossy(&run.stdout);
        let label = format!("{} [{}]", filename, opt_flag);
        if let Err(e) = match_checks(&checks, &stdout, &label, "CHECK") {
            let _ = fs::remove_file(&binary);
            let _ = fs::remove_dir_all(&sandbox);
            return Err(e);
        }
        if let Err(e) = match_checks(&stderr_checks, &stderr, &label, "STDERR_CHECK") {
            let _ = fs::remove_file(&binary);
            let _ = fs::remove_dir_all(&sandbox);
            return Err(e);
        }
        let _ = fs::remove_file(&binary);
        let _ = fs::remove_dir_all(&sandbox);

        // IR shape assertions: only at -O0, where the IR is
        // stable. Optimization passes (mem2reg, LICM, CSE, etc.)
        // erase the very shape we want to pin, so running these
        // at -O1+ would always fail. The runtime CHECKs above
        // continue to run at every level.
        if !ir_checks.is_empty() && opt_flag == "-O0" {
            let ir_dest = std::env::temp_dir().join(format!(
                "afs_test_{}_ir.txt",
                source.file_stem().unwrap().to_str().unwrap(),
            ));
            let ir_compile = Command::new(compiler)
                .args([
                    source.to_str().unwrap(),
                    "-O0",
                    "--emit-ir",
                    "-o",
                    ir_dest.to_str().unwrap(),
                ])
                .output()
                .map_err(|e| format!("{}: cannot run --emit-ir: {}", filename, e))?;
            if !ir_compile.status.success() {
                let stderr = String::from_utf8_lossy(&ir_compile.stderr);
                return Err(format!(
                    "{} [{}]: --emit-ir compilation failed:\n{}",
                    filename, opt_flag, stderr,
                ));
            }
            let ir_text = fs::read_to_string(&ir_dest)
                .map_err(|e| format!("{}: cannot read IR: {}", filename, e))?;
            let _ = fs::remove_file(&ir_dest);
            match_ir_checks(&ir_checks, &ir_text, &label)?;
        }

        Ok(())
    };

    let result = inner();
    match (xfail_reason, result) {
        (None, Ok(())) => TestOutcome::Pass,
        (None, Err(e)) => TestOutcome::Fail(e),
        (Some(reason), Err(e)) => TestOutcome::Xfail(format!("{}: {}", reason, e)),
        (Some(reason), Ok(())) => TestOutcome::Xpass(format!(
            "{} [{}]: marked XFAIL ({}) but unexpectedly passed — \
             remove the XFAIL annotation",
            filename, opt_flag, reason,
        )),
    }
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
    let mut xfailed = 0;

    for source in &sources {
        let name = source.file_name().unwrap().to_str().unwrap();
        match run_test(&compiler, source, opt_flag) {
            TestOutcome::Pass => {
                passed += 1;
                eprintln!("  PASS  [{}]: {}", opt_flag, name);
            }
            TestOutcome::Xfail(detail) => {
                xfailed += 1;
                // Print the first line of the detail so we know what
                // the underlying failure looked like, in case the bug
                // class shifts.
                let one_line = detail.lines().next().unwrap_or("");
                eprintln!("  XFAIL [{}]: {} — {}", opt_flag, name, one_line);
            }
            TestOutcome::Xpass(msg) => {
                eprintln!("  XPASS [{}]: {}", opt_flag, name);
                failures.push(msg);
            }
            TestOutcome::Fail(msg) => {
                eprintln!("  FAIL  [{}]: {}", opt_flag, name);
                failures.push(msg);
            }
        }
    }

    eprintln!(
        "\n[{}] {} passed, {} xfailed, {} failed out of {} test programs",
        opt_flag,
        passed,
        xfailed,
        failures.len(),
        sources.len(),
    );

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
    assert!(
        source.exists(),
        "two_loops.f90 missing — needed for determinism check"
    );

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
    assert!(
        source.exists(),
        "module_init.f90 missing — needed for determinism check"
    );

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

#[test]
fn extract_exit_code_accepts_integer_annotation() {
    let source = "! EXIT_CODE: 17\nprogram t\nend program t\n";
    assert_eq!(extract_exit_code(source, "inline.f90").unwrap(), Some(17));
}

#[test]
fn extract_exit_code_rejects_multiple_annotations() {
    let source = "! EXIT_CODE: 1\n! EXIT_CODE: 2\nprogram t\nend program t\n";
    let err = extract_exit_code(source, "inline.f90").unwrap_err();
    assert!(err.contains("multiple EXIT_CODE annotations"));
}

#[test]
fn match_checks_reports_stderr_check_failures_by_name() {
    let checks = vec![Check {
        line_num: 1,
        pattern: "ERROR STOP".into(),
    }];
    let err = match_checks(
        &checks,
        "different stderr",
        "inline.f90 [O0]",
        "STDERR_CHECK",
    )
    .unwrap_err();
    assert!(err.contains("STDERR_CHECK failed"));
}

#[test]
fn stderr_and_exit_code_annotations_allow_error_stop() {
    let compiler = find_compiler();
    let test_dir = find_test_programs();
    let source = test_dir.join("error_stop_status.f90");
    assert!(
        source.exists(),
        "error_stop_status.f90 missing — needed for stderr/exit-code harness coverage"
    );

    match run_test(&compiler, &source, "-O0") {
        TestOutcome::Pass => {}
        other => panic!("error_stop_status.f90 should pass, got {:?}", other),
    }
}
