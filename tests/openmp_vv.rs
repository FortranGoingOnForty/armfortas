//! Pinned, unchanged host tests from the OpenMP Validation and Verification suite.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct Case<'a> {
    id: &'a str,
    upstream_path: &'a str,
    local_source: &'a str,
    support_header: &'a str,
    operation: &'a str,
    upstream_expect: &'a str,
    version: &'a str,
    coverage: &'a str,
    armfortas_state: &'a str,
    sha256: &'a str,
}

fn compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary not built for this test profile")
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openmp-vv")
}

fn unique_dir(stem: &str) -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "afs_openmp_vv_{stem}_{}_{}",
        std::process::id(),
        id
    ));
    std::fs::create_dir_all(&path).expect("cannot create OpenMP V&V test directory");
    path
}

fn cases() -> Vec<Case<'static>> {
    const MANIFEST: &str = include_str!("fixtures/openmp-vv/manifest.tsv");
    const HEADER: &str = "id\tupstream_path\tlocal_source\tsupport_header\toperation\tupstream_expect\tversion\tcoverage\tarmfortas_state\tsha256";
    let mut lines = MANIFEST.lines();
    assert_eq!(lines.next(), Some(HEADER), "unexpected manifest schema");
    lines
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            assert_eq!(fields.len(), 10, "malformed manifest row: {line}");
            let case = Case {
                id: fields[0],
                upstream_path: fields[1],
                local_source: fields[2],
                support_header: fields[3],
                operation: fields[4],
                upstream_expect: fields[5],
                version: fields[6],
                coverage: fields[7],
                armfortas_state: fields[8],
                sha256: fields[9],
            };
            assert!(matches!(case.operation, "compile" | "link" | "run"));
            assert_eq!(case.upstream_expect, "success", "{case:?}");
            assert_eq!(case.armfortas_state, "pass", "{case:?}");
            assert!(
                !case.version.is_empty() && !case.coverage.is_empty(),
                "{case:?}"
            );
            assert_eq!(case.sha256.len(), 64, "{case:?}");
            case
        })
        .collect()
}

fn sha256(path: &Path) -> String {
    let output = Command::new("sha256sum").arg(path).output().or_else(|_| {
        Command::new("shasum")
            .args(["-a", "256"])
            .arg(path)
            .output()
    });
    let output = output.expect("neither sha256sum nor shasum is available");
    assert!(output.status.success(), "failed to hash {}", path.display());
    String::from_utf8(output.stdout)
        .expect("checksum output is not UTF-8")
        .split_whitespace()
        .next()
        .expect("checksum output is empty")
        .to_string()
}

#[test]
fn pinned_host_cases_build_and_run_at_o0_and_o3() {
    let root = fixture_root();
    let build = unique_dir("host");
    let runtime_cache = build.join("runtime-cache");
    let native = armfortas::testing::native_e2e_support();
    if let Err(reason) = &native {
        armfortas::testing::report_harness_skip(
            "openmp_vv",
            "pinned_host_cases_build_and_run_at_o0_and_o3",
            1,
            &format!("pinned V&V run operation unavailable: {reason}"),
        );
    }

    for case in cases() {
        let source = root.join(case.local_source);
        let support_header = root.join(case.support_header);
        assert!(source.is_file(), "missing pinned source for {case:?}");
        assert!(
            support_header.is_file(),
            "missing support header for {case:?}"
        );
        assert_eq!(
            sha256(&source),
            case.sha256,
            "pinned source drifted from {}",
            case.upstream_path
        );

        for opt in ["-O0", "-O3"] {
            let compile_only = case.operation == "compile" || native.is_err();
            let output = build.join(format!(
                "{}-{}{}",
                case.id,
                opt.trim_start_matches('-').to_ascii_lowercase(),
                if compile_only { ".o" } else { "" }
            ));
            let mut command = Command::new(compiler());
            command
                .current_dir(&build)
                .args(["-fopenmp", opt, "-I"])
                .arg(
                    support_header
                        .parent()
                        .expect("support header has no parent"),
                );
            if compile_only {
                command.arg("-c");
            }
            let compile = command
                .arg(&source)
                .args(["-o"])
                .arg(&output)
                .env("AFS_RUNTIME_CACHE", &runtime_cache)
                .output()
                .expect("failed to launch armfortas");
            assert!(
                compile.status.success(),
                "OpenMP V&V case {} ({}) failed to {} at {opt}:\n{}",
                case.id,
                case.upstream_path,
                if compile_only { "compile" } else { "link" },
                String::from_utf8_lossy(&compile.stderr)
            );

            if case.operation == "run" && native.is_ok() {
                let run = Command::new(&output)
                    .env("OMP_NUM_THREADS", "4")
                    .output()
                    .expect("failed to launch pinned OpenMP V&V case");
                assert!(
                    run.status.success(),
                    "OpenMP V&V case {} failed at {opt}:\nstdout:\n{}\nstderr:\n{}",
                    case.id,
                    String::from_utf8_lossy(&run.stdout),
                    String::from_utf8_lossy(&run.stderr)
                );
            }
        }
    }
    let _ = std::fs::remove_dir_all(&build);
}
