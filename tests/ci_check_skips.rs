use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const MACOS_SUITES: &[&str] = &[
    "x86_emit_golden",
    "abi_differential",
    "x86_object_smoke",
    "x86_afs_as_differential",
    "elf_link_e2e",
    "elf_static_link",
];

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ci/check_skips.sh")
}

fn log_path(case: &str) -> PathBuf {
    std::env::temp_dir().join(format!("afs-check-skips-{}-{case}.log", std::process::id()))
}

fn skip_line(suite: &str, count: usize) -> String {
    format!("HARNESS_SKIP suite={suite} test=platform_gate count={count} reason=\"test fixture\"\n")
}

fn valid_macos_log() -> String {
    MACOS_SUITES
        .iter()
        .map(|suite| skip_line(suite, 1))
        .collect()
}

fn run_checker(case: &str, log: &str) -> Output {
    let path = log_path(case);
    fs::write(&path, log).expect("write skip log");
    let output = Command::new("sh")
        .arg(script())
        .arg(&path)
        .arg("macos")
        .output()
        .expect("run skip checker");
    let _ = fs::remove_file(path);
    output
}

#[test]
fn macos_accepts_the_complete_positive_allowlist() {
    let output = run_checker("valid", &valid_macos_log());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn macos_rejects_an_empty_log() {
    let output = run_checker("empty", "");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no HARNESS_SKIP lines"));
}

#[test]
fn macos_rejects_zero_counts() {
    let log = valid_macos_log().replacen("count=1", "count=0", 1);
    let output = run_checker("zero", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing/zero count"));
}

#[test]
fn macos_rejects_a_missing_expected_suite() {
    let log = valid_macos_log().replace(&skip_line(MACOS_SUITES[0], 1), "");
    let output = run_checker("missing", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains(MACOS_SUITES[0]));
}

#[test]
fn macos_rejects_an_unexpected_suite() {
    let log = valid_macos_log() + &skip_line("native_suite", 1);
    let output = run_checker("unexpected", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("native_suite"));
}
