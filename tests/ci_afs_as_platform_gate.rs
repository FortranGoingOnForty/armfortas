use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

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

fn skip_line(suite: &str, reason: &str) -> String {
    format!(
        "HARNESS_SKIP suite={suite} test=platform_gate count=1 reason=\"{reason}\"\n\
         test result: ok. 1 passed; 0 failed; 0 ignored; 0 measured; 0 filtered out\n"
    )
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
    let log = skip_line("elf_differential", "no GNU assembler on this host");
    let output = run_skip_checker("macos-valid", "macos-arm64", &log);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn macos_arm64_rejects_machine_readable_native_skips() {
    let log = skip_line("differential_harness", "needs a macOS arm64 host toolchain");
    let output = run_skip_checker("macos-native-skip", "macos-arm64", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("macOS arm64 host toolchain"));
}

#[test]
fn macos_arm64_rejects_legacy_plain_text_native_skips() {
    let log = format!(
        "{}skipping: clang_probe_dashboard requires a macOS arm64 host toolchain\n",
        skip_line("elf_differential", "no GNU assembler on this host")
    );
    let output = run_skip_checker("macos-legacy-skip", "macos-arm64", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("macOS arm64 host toolchain"));
}

#[test]
fn linux_x86_64_accepts_only_off_platform_macho_skips() {
    let log = skip_line("differential_harness", "needs a macOS arm64 host toolchain");
    let output = run_skip_checker("linux-valid", "linux-x86_64", &log);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn linux_x86_64_rejects_missing_native_assembler() {
    let log = skip_line("x86_assemble_differential", "no GNU assembler on this host");
    let output = run_skip_checker("linux-native-skip", "linux-x86_64", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no GNU assembler"));
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
