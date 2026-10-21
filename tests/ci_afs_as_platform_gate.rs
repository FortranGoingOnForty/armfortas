use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const MACOS_MANIFEST: &str = include_str!("../ci/expected_afs_as_skips_macos-arm64.txt");
const LINUX_MANIFEST: &str = include_str!("../ci/expected_afs_as_skips_linux-x86_64.txt");

#[derive(Clone, Debug)]
struct SkipRecord {
    suite: String,
    test: String,
    count: usize,
}

fn script(name: &str) -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
        .join("ci")
        .join(name)
}

fn log_path(case: &str) -> PathBuf {
    std::env::temp_dir().join(format!(
        "afs-as-platform-gate-{}-{case}.log",
        std::process::id()
    ))
}

fn run_skip_checker(case: &str, profile: &str, log: &str) -> Output {
    let path = log_path(case);
    fs::write(&path, log).expect("write afs-as test log");
    let output = Command::new("sh")
        .arg(script("check_afs_as_skips.sh"))
        .arg(&path)
        .arg(profile)
        .output()
        .expect("run afs-as skip checker");
    let _ = fs::remove_file(path);
    output
}

fn manifest_records(manifest: &str) -> Vec<SkipRecord> {
    manifest
        .lines()
        .filter_map(|line| {
            let line = line.trim();
            if line.is_empty() || line.starts_with('#') {
                return None;
            }
            let mut fields = line.split_whitespace();
            let record = SkipRecord {
                suite: fields.next().expect("manifest suite").to_owned(),
                test: fields.next().expect("manifest test").to_owned(),
                count: fields
                    .next()
                    .expect("manifest count")
                    .parse()
                    .expect("positive manifest count"),
            };
            assert!(fields.next().is_none(), "extra manifest fields: {line}");
            Some(record)
        })
        .collect()
}

fn profile_records(profile: &str) -> Vec<SkipRecord> {
    match profile {
        "macos-arm64" => manifest_records(MACOS_MANIFEST),
        "linux-x86_64" => manifest_records(LINUX_MANIFEST),
        other => panic!("unknown test profile {other}"),
    }
}

fn skip_line(record: &SkipRecord, reason: &str) -> String {
    format!(
        "HARNESS_SKIP suite={} test={} count={} reason=\"{reason}\"\n",
        record.suite, record.test, record.count
    )
}

fn valid_log(profile: &str) -> String {
    let mut log: String = profile_records(profile)
        .iter()
        .map(|record| skip_line(record, "off-platform prerequisite"))
        .collect();
    log.push_str("test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n");
    log
}

#[test]
fn host_assertion_accepts_the_observed_host() {
    let host_os = Command::new("uname")
        .arg("-s")
        .output()
        .expect("read host OS");
    let host_arch = Command::new("uname")
        .arg("-m")
        .output()
        .expect("read host architecture");
    assert!(host_os.status.success());
    assert!(host_arch.status.success());

    let output = Command::new("sh")
        .arg(script("assert_host.sh"))
        .arg(String::from_utf8_lossy(&host_os.stdout).trim())
        .arg(String::from_utf8_lossy(&host_arch.stdout).trim())
        .output()
        .expect("run host assertion");

    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn host_assertion_rejects_a_different_host() {
    let output = Command::new("sh")
        .arg(script("assert_host.sh"))
        .args(["DefinitelyNotThisOS", "definitely-not-this-arch"])
        .output()
        .expect("run host assertion");

    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("expected host"));
}

#[test]
fn macos_arm64_accepts_only_off_platform_elf_skips() {
    let log = valid_log("macos-arm64");
    let output = run_skip_checker("macos-valid", "macos-arm64", &log);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn macos_arm64_rejects_machine_readable_native_skips() {
    let log = valid_log("macos-arm64").replacen(
        "reason=\"off-platform prerequisite\"",
        "reason=\"needs a macOS arm64 host toolchain\"",
        1,
    );
    let output = run_skip_checker("macos-native-skip", "macos-arm64", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("macOS arm64 host toolchain"));
}

#[test]
fn macos_arm64_rejects_legacy_plain_text_native_skips() {
    let log = format!(
        "{}skipping: clang_probe_dashboard requires a macOS arm64 host toolchain\n",
        valid_log("macos-arm64")
    );
    let output = run_skip_checker("macos-legacy-skip", "macos-arm64", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("macOS arm64 host toolchain"));
}

#[test]
fn linux_x86_64_accepts_only_off_platform_macho_skips() {
    let log = valid_log("linux-x86_64");
    let output = run_skip_checker("linux-valid", "linux-x86_64", &log);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn linux_x86_64_rejects_missing_native_assembler() {
    let log = valid_log("linux-x86_64").replacen(
        "reason=\"off-platform prerequisite\"",
        "reason=\"no GNU assembler on this host\"",
        1,
    );
    let output = run_skip_checker("linux-native-skip", "linux-x86_64", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no GNU assembler"));
}

#[test]
fn exact_gate_rejects_missing_unexpected_changed_and_duplicate_records() {
    let records = profile_records("macos-arm64");
    let first = &records[0];
    let valid = valid_log("macos-arm64");

    let missing = valid.replacen(&skip_line(first, "off-platform prerequisite"), "", 1);
    let output = run_skip_checker("missing-record", "macos-arm64", &missing);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("missing records"));

    let unexpected = format!(
        "{}HARNESS_SKIP suite=invented test=case count=1 reason=\"off-platform prerequisite\"\n",
        valid
    );
    let output = run_skip_checker("unexpected-record", "macos-arm64", &unexpected);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected records"));

    let changed = valid.replacen(
        &format!("count={}", first.count),
        &format!("count={}", first.count + 1),
        1,
    );
    let output = run_skip_checker("changed-count", "macos-arm64", &changed);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("manifest mismatch"));

    let duplicate = valid.clone() + &skip_line(first, "off-platform prerequisite");
    let output = run_skip_checker("duplicate-record", "macos-arm64", &duplicate);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate"));
}

#[test]
fn exact_gate_rejects_interleaved_skip_records() {
    let log = valid_log("macos-arm64").replacen(
        "HARNESS_SKIP suite=",
        "HARNESS_SKIP suite=test output interleaved ",
        1,
    );
    let output = run_skip_checker("interleaved", "macos-arm64", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("malformed or interleaved"));
}

#[test]
fn skip_gate_rejects_captured_or_missing_skip_evidence() {
    let output = run_skip_checker(
        "captured",
        "macos-arm64",
        "test result: ok. 2 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n",
    );
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no HARNESS_SKIP"));
}
