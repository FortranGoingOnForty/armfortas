use std::fs;
use std::path::PathBuf;
use std::process::{Command, Output};

const MACOS_MANIFEST: &str = include_str!("../ci/expected_skips_macos.txt");
const POSIX_ELF_MANIFEST: &str = include_str!("../ci/expected_skips_posix-elf.txt");
const POSIX_ELF_MUSL_EXTRA_MANIFEST: &str =
    include_str!("../ci/expected_skips_posix-elf-musl-extra.txt");

#[derive(Clone, Debug)]
struct SkipRecord {
    suite: String,
    test: String,
    count: usize,
}

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ci/check_skips.sh")
}

fn log_path(case: &str) -> PathBuf {
    std::env::temp_dir().join(format!("afs-check-skips-{}-{case}.log", std::process::id()))
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

fn macos_records() -> Vec<SkipRecord> {
    manifest_records(MACOS_MANIFEST)
}

fn skip_line(record: &SkipRecord) -> String {
    format!(
        "HARNESS_SKIP suite={} test={} count={} reason=\"test fixture\"\n",
        record.suite, record.test, record.count
    )
}

fn valid_macos_log() -> String {
    macos_records().iter().map(skip_line).collect()
}

fn run_checker(case: &str, profile: &str, log: &str) -> Output {
    let path = log_path(case);
    fs::write(&path, log).expect("write skip log");
    let output = Command::new("sh")
        .arg(script())
        .arg(&path)
        .arg(profile)
        .output()
        .expect("run skip checker");
    let _ = fs::remove_file(path);
    output
}

fn run_macos_checker(case: &str, log: &str) -> Output {
    run_checker(case, "macos", log)
}

#[test]
fn macos_accepts_the_complete_exact_manifest() {
    let output = run_macos_checker("valid", &valid_macos_log());
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn macos_rejects_an_empty_log() {
    let output = run_macos_checker("empty", "");
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("no HARNESS_SKIP records"));
}

#[test]
fn macos_rejects_zero_counts() {
    let first = &macos_records()[0];
    let log = valid_macos_log().replacen(&format!("count={}", first.count), "count=0", 1);
    let output = run_macos_checker("zero", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("non-positive skip count"));
}

#[test]
fn macos_rejects_a_missing_expected_record() {
    let first = &macos_records()[0];
    let log = valid_macos_log().replacen(&skip_line(first), "", 1);
    let output = run_macos_checker("missing", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains(&first.test));
}

#[test]
fn macos_rejects_an_unexpected_suite() {
    let unexpected = SkipRecord {
        suite: "native_suite".to_owned(),
        test: "invented_skip".to_owned(),
        count: 1,
    };
    let log = valid_macos_log() + &skip_line(&unexpected);
    let output = run_macos_checker("unexpected", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("native_suite"));
}

#[test]
fn macos_rejects_a_duplicate_skip_record() {
    let log = valid_macos_log() + &skip_line(&macos_records()[0]);
    let output = run_macos_checker("duplicate", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("duplicate"));
}

#[test]
fn macos_rejects_an_unexpected_test_in_an_expected_suite() {
    let first = &macos_records()[0];
    let log = valid_macos_log().replacen(&format!("test={}", first.test), "test=invented_skip", 1);
    let output = run_macos_checker("unexpected-test", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("invented_skip"));
}

#[test]
fn macos_rejects_an_inflated_count() {
    let first = &macos_records()[0];
    let log = valid_macos_log().replacen(&format!("count={}", first.count), "count=999", 1);
    let output = run_macos_checker("inflated-count", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("count=999"));
}

#[test]
fn macos_recognizes_a_record_interleaved_with_libtest_output() {
    let log = valid_macos_log().replacen("HARNESS_SKIP ", "test fixture_name ... HARNESS_SKIP ", 1);
    let output = run_macos_checker("inline-record", &log);
    assert!(
        output.status.success(),
        "{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn all_profiles_accept_their_exact_committed_manifests() {
    let profiles = [
        ("macos", manifest_records(MACOS_MANIFEST)),
        ("posix-elf", manifest_records(POSIX_ELF_MANIFEST)),
        (
            "posix-elf-musl",
            [
                manifest_records(POSIX_ELF_MANIFEST),
                manifest_records(POSIX_ELF_MUSL_EXTRA_MANIFEST),
            ]
            .concat(),
        ),
    ];

    for (profile, records) in profiles {
        let log: String = records.iter().map(skip_line).collect();
        let output = run_checker(profile, profile, &log);
        assert!(
            output.status.success(),
            "{profile}: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }
}

#[test]
fn macos_rejects_a_malformed_record_even_when_the_manifest_is_complete() {
    let log = valid_macos_log()
        + "HARNESS_SKIP suite=abi_differential count=1 reason=\"missing test field\"\n";
    let output = run_macos_checker("malformed", &log);
    assert!(!output.status.success());
    assert!(String::from_utf8_lossy(&output.stderr).contains("malformed HARNESS_SKIP"));
}
