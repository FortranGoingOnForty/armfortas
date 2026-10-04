//! Pinned, unchanged examples from the OpenMP Architecture Review Board.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

#[derive(Debug)]
struct Case<'a> {
    id: &'a str,
    upstream_path: &'a str,
    local_source: &'a str,
    source_form: &'a str,
    operation: &'a str,
    upstream_expect: &'a str,
    version: &'a str,
    coverage: &'a str,
    armfortas_state: &'a str,
}

fn compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary not built for this test profile")
}

fn fixture_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("tests/fixtures/openmp-official")
}

fn unique_dir(stem: &str) -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "afs_openmp_official_{stem}_{}_{}",
        std::process::id(),
        id
    ));
    std::fs::create_dir_all(&path).expect("cannot create OpenMP example test directory");
    path
}

fn cases() -> Vec<Case<'static>> {
    const MANIFEST: &str = include_str!("fixtures/openmp-official/manifest.tsv");
    const HEADER: &str = "id\tupstream_path\tlocal_source\tsource_form\toperation\tupstream_expect\tversion\tcoverage\tarmfortas_state";
    let mut lines = MANIFEST.lines();
    assert_eq!(lines.next(), Some(HEADER), "unexpected manifest schema");
    lines
        .filter(|line| !line.trim().is_empty())
        .map(|line| {
            let fields = line.split('\t').collect::<Vec<_>>();
            assert_eq!(fields.len(), 9, "malformed manifest row: {line}");
            let case = Case {
                id: fields[0],
                upstream_path: fields[1],
                local_source: fields[2],
                source_form: fields[3],
                operation: fields[4],
                upstream_expect: fields[5],
                version: fields[6],
                coverage: fields[7],
                armfortas_state: fields[8],
            };
            assert!(matches!(case.source_form, "fixed" | "free"), "{case:?}");
            assert!(
                matches!(case.operation, "compile" | "link" | "run"),
                "{case:?}"
            );
            assert_eq!(case.upstream_expect, "success", "{case:?}");
            assert_eq!(case.armfortas_state, "pass", "{case:?}");
            assert!(
                !case.version.is_empty() && !case.coverage.is_empty(),
                "{case:?}"
            );
            case
        })
        .collect()
}

fn assert_pinned_source(case: &Case<'_>, source: &Path) {
    let text = std::fs::read_to_string(source)
        .unwrap_or_else(|error| panic!("cannot read {}: {error}", source.display()));
    assert!(
        text.contains(&format!("@@name:\t{}", case.id)),
        "{} is not the pinned {} source from {}",
        source.display(),
        case.id,
        case.upstream_path
    );
    assert!(
        text.contains(&format!("@@operation:\t{}", case.operation)),
        "operation metadata drifted for {}",
        case.id
    );
    assert!(
        text.contains(&format!("@@version:\t{}", case.version)),
        "version metadata drifted for {}",
        case.id
    );
}

#[test]
fn selected_official_examples_build_at_o0_and_o3() {
    let root = fixture_root();
    let build = unique_dir("compile");
    let runtime_cache = build.join("runtime-cache");
    for case in cases() {
        let source = root.join(case.local_source);
        assert_pinned_source(&case, &source);
        for opt in ["-O0", "-O3"] {
            let output = build.join(format!(
                "{}-{}{}",
                case.id,
                opt.trim_start_matches('-').to_ascii_lowercase(),
                if case.operation == "compile" {
                    ".o"
                } else {
                    ""
                }
            ));
            let mut command = Command::new(compiler());
            command.current_dir(&build).args(["-fopenmp", opt]);
            if case.operation == "compile" {
                command.arg("-c");
            }
            let result = command
                .arg(&source)
                .args(["-o"])
                .arg(&output)
                .env("AFS_RUNTIME_CACHE", &runtime_cache)
                .output()
                .expect("failed to launch armfortas");
            assert!(
                result.status.success(),
                "official example {} ({}) failed to {} at {opt}:\n{}",
                case.id,
                case.upstream_path,
                case.operation,
                String::from_utf8_lossy(&result.stderr)
            );
        }
    }
    let _ = std::fs::remove_dir_all(&build);
}

#[test]
fn selected_official_run_examples_pass_at_o0_and_o3() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=openmp_official_examples test=selected_official_run_examples_pass_at_o0_and_o3 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let root = fixture_root();
    let build = unique_dir("run");
    let runtime_cache = build.join("runtime-cache");
    for case in cases().into_iter().filter(|case| case.operation == "run") {
        let source = root.join(case.local_source);
        for opt in ["-O0", "-O3"] {
            let executable = build.join(format!(
                "{}-{}",
                case.id,
                opt.trim_start_matches('-').to_ascii_lowercase()
            ));
            let compile = Command::new(compiler())
                .current_dir(&build)
                .args(["-fopenmp", opt])
                .arg(&source)
                .args(["-o"])
                .arg(&executable)
                .env("AFS_RUNTIME_CACHE", &runtime_cache)
                .output()
                .expect("failed to launch armfortas");
            assert!(
                compile.status.success(),
                "official run example {} failed to link at {opt}:\n{}",
                case.id,
                String::from_utf8_lossy(&compile.stderr)
            );
            let run = Command::new(&executable)
                .env("OMP_NUM_THREADS", "4")
                .output()
                .expect("failed to launch official OpenMP example");
            assert!(
                run.status.success(),
                "official run example {} failed at {opt}:\nstdout:\n{}\nstderr:\n{}",
                case.id,
                String::from_utf8_lossy(&run.stdout),
                String::from_utf8_lossy(&run.stderr)
            );
            if case.id == "private.1" {
                assert_eq!(
                    String::from_utf8_lossy(&run.stdout)
                        .split_whitespace()
                        .collect::<Vec<_>>(),
                    ["1", "2"]
                );
            }
        }
    }
    let _ = std::fs::remove_dir_all(&build);
}
