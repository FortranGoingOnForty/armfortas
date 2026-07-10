//! Determinism sweep — compile every test program twice at -O2 and
//! verify byte-identical assembly output. Catches non-deterministic
//! codegen (HashMap iteration order, unstable sorts, stale regalloc
//! state) across the entire test corpus, not just the subset with
//! explicit REPRO_CHECK annotations.
//!
//! Multi-file tests (those containing `!--- file:` markers) and
//! error-expected tests are skipped — they either need special
//! handling or don't produce assembly.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_ID: AtomicUsize = AtomicUsize::new(0);

fn find_compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary not built for this test profile")
}

fn find_test_programs() -> PathBuf {
    for c in &["test_programs", "../test_programs"] {
        let p = PathBuf::from(c);
        if p.is_dir() {
            return p;
        }
    }
    panic!("cannot find test_programs/ directory");
}

fn unique_path(prefix: &str, ext: &str) -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "afs_det_{}_{}_{}{}",
        prefix,
        std::process::id(),
        id,
        ext
    ))
}

/// Compile a source to assembly, returning the bytes.
/// Returns None if compilation fails (e.g., error-expected tests).
fn compile_to_asm(compiler: &Path, source: &Path, opt: &str) -> Option<Vec<u8>> {
    let out = unique_path("sweep", ".s");
    let result = Command::new(compiler)
        .args([
            source.to_str().unwrap(),
            opt,
            "-S",
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("compiler launch failed");
    if !result.status.success() {
        let _ = fs::remove_file(&out);
        return None;
    }
    let bytes = fs::read(&out).expect("cannot read .s output");
    let _ = fs::remove_file(&out);
    Some(bytes)
}

fn should_skip(source_text: &str) -> bool {
    // Skip multi-file tests.
    if source_text.contains("!--- file:") {
        return true;
    }
    // Skip error-expected tests (they're supposed to fail compilation).
    if source_text.contains("! ERROR_EXPECTED:") {
        return true;
    }
    // Skip XFAIL tests (they have known failures).
    if source_text.contains("! XFAIL:") {
        return true;
    }
    false
}

fn assert_repeated_asm_identical(compiler: &Path, source: &Path, opt: &str, runs: usize) {
    assert!(runs >= 2, "determinism checks need at least two runs");
    let baseline = compile_to_asm(compiler, source, opt)
        .unwrap_or_else(|| panic!("{} {} compile failed", source.display(), opt));
    for run in 2..=runs {
        let current = compile_to_asm(compiler, source, opt).unwrap_or_else(|| {
            panic!("{} {} compile failed on run {}", source.display(), opt, run)
        });
        assert_eq!(
            baseline,
            current,
            "{} {} assembly differs on run {} of {}",
            source.display(),
            opt,
            run,
            runs
        );
    }
}

#[test]
fn audit_shaped_programs_deterministic_eight_runs_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=determinism_sweep test=audit_shaped_programs_deterministic_eight_runs_at_o2 count=2 reason=\"{}\"",
            reason
        );
        return;
    }

    let compiler = find_compiler();
    let test_dir = find_test_programs();
    for name in [
        "ar13_unswitch_deterministic.f90",
        "ar13_fgof_watch_deterministic.f90",
    ] {
        assert_repeated_asm_identical(&compiler, &test_dir.join(name), "-O2", 8);
    }
}

#[test]
fn all_programs_deterministic_at_o2() {
    let test_dir = find_test_programs();

    let mut sources: Vec<PathBuf> = fs::read_dir(&test_dir)
        .expect("cannot read test_programs/")
        .filter_map(|e| e.ok())
        .map(|e| e.path())
        .filter(|p| p.extension().map(|e| e == "f90").unwrap_or(false))
        .collect();
    sources.sort();
    assert!(
        !sources.is_empty(),
        "no .f90 sources discovered in {} — discovery must stay a hard failure even when skipping",
        test_dir.display()
    );

    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=determinism_sweep test=all_programs_deterministic_at_o2 count={} reason=\"{}\"",
            sources.len(),
            reason
        );
        return;
    }
    let compiler = find_compiler();

    let mut checked = 0;
    let mut skipped = 0;
    let mut failures = Vec::new();

    for source in &sources {
        let name = source.file_name().unwrap().to_str().unwrap();
        let text = fs::read_to_string(source).unwrap();
        if should_skip(&text) {
            skipped += 1;
            continue;
        }

        let first = match compile_to_asm(&compiler, source, "-O2") {
            Some(bytes) => bytes,
            None => {
                // Compilation failed — this test has other issues; skip.
                skipped += 1;
                continue;
            }
        };
        let second = compile_to_asm(&compiler, source, "-O2")
            .expect("second compile failed but first succeeded — flaky");

        if first != second {
            failures.push(format!(
                "{}: assembly differs between two -O2 compilations ({} vs {} bytes)",
                name,
                first.len(),
                second.len(),
            ));
        } else {
            checked += 1;
        }
    }

    eprintln!(
        "\nDeterminism sweep: {} checked, {} skipped, {} failures out of {} programs",
        checked,
        skipped,
        failures.len(),
        sources.len(),
    );

    if !failures.is_empty() {
        panic!("Determinism failures:\n{}", failures.join("\n"),);
    }

    // Sanity: we should have checked a substantial portion.
    assert!(
        checked >= 150,
        "only {} programs checked — expected at least 150",
        checked,
    );
}
