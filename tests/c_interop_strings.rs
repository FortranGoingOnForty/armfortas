//! Differential C-interop test for the l06 string intrinsics
//! (C_F_STRPOINTER, F_C_STRING, C_F_POINTER LOWER). A C translation unit
//! compiled with the system `clang` hands strings/arrays across the
//! boundary; the armfortas-compiled Fortran main checks its view against
//! C's own strlen/strcmp/byte access. Pattern mirrors
//! `tests/i128_cross_object.rs`: clang for the C side, the armfortas
//! driver for compiling and linking (the driver owns the link line on
//! every platform — no `xcrun`/`ld` hardcoding here).
//!
//! Skips with an explicit HARNESS_SKIP line (never silently green) when
//! `clang` is absent.

use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from("tests/fixtures").join(name);
    assert!(path.exists(), "missing test fixture {}", path.display());
    path
}

fn unique_temp_path(stem: &str, ext: &str) -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "afs_cis_{}_{}_{}{}",
        std::process::id(),
        id,
        stem,
        ext
    ))
}

fn clang_available() -> bool {
    Command::new("clang")
        .arg("--version")
        .output()
        .map(|o| o.status.success())
        .unwrap_or(false)
}

fn find_compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary not built for this test profile")
}

fn find_runtime_lib() -> PathBuf {
    armfortas::testing::built_runtime_archive()
        .expect("libarmfortas_rt.a not built for this test profile")
}

fn run(cmd: &mut Command, what: &str) {
    let out = cmd
        .output()
        .unwrap_or_else(|e| panic!("cannot launch {what}: {e}"));
    assert!(
        out.status.success(),
        "{what} failed:\n{}\n{}",
        String::from_utf8_lossy(&out.stdout),
        String::from_utf8_lossy(&out.stderr)
    );
}

fn run_case(opt_flag: &str) {
    let cobj = unique_temp_path(
        &format!("helper_{}", opt_flag.trim_start_matches('-')),
        ".o",
    );
    let fobj = unique_temp_path(&format!("main_{}", opt_flag.trim_start_matches('-')), ".o");
    let bin = unique_temp_path(&format!("bin_{}", opt_flag.trim_start_matches('-')), "");

    // C side via clang.
    run(
        Command::new("clang").args([
            "-fPIC",
            "-c",
            fixture("c_interop_strings_helper.c").to_str().unwrap(),
            "-o",
            cobj.to_str().unwrap(),
        ]),
        "clang compile",
    );

    // Fortran side via the armfortas driver.
    let afs = find_compiler();
    run(
        Command::new(&afs).args([
            "--std=f2023",
            opt_flag,
            "-c",
            fixture("c_interop_strings_main.f90").to_str().unwrap(),
            "-o",
            fobj.to_str().unwrap(),
        ]),
        "armfortas compile",
    );

    // Link both objects + runtime through the driver.
    let rt = find_runtime_lib();
    run(
        Command::new(&afs)
            .arg(&fobj)
            .arg(&cobj)
            .arg(&rt)
            .arg("-o")
            .arg(&bin),
        "driver link",
    );

    let out = Command::new(&bin)
        .output()
        .unwrap_or_else(|e| panic!("cannot run {}: {e}", bin.display()));
    let stdout = String::from_utf8_lossy(&out.stdout);
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        out.status.success(),
        "differential binary failed at {opt_flag} (exit {:?}):\nstdout: {stdout}\nstderr: {stderr}",
        out.status.code()
    );
    assert!(
        stdout.contains("all checks passed"),
        "missing success marker at {opt_flag}:\nstdout: {stdout}\nstderr: {stderr}"
    );

    let _ = fs::remove_file(&cobj);
    let _ = fs::remove_file(&fobj);
    let _ = fs::remove_file(&bin);
}

#[test]
fn c_interop_strings_differential() {
    if !clang_available() {
        eprintln!(
            "\nHARNESS_SKIP suite=c_interop_strings test=c_interop_strings_differential \
             count=2 reason=\"clang not found on PATH\""
        );
        return;
    }
    for opt in ["-O0", "-O2"] {
        run_case(opt);
    }
}
