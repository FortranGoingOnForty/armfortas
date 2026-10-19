use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

const PROGRAMS: &[&str] = &[
    "array_bulk_kernels.f90",
    "module_init.f90",
    "two_loops.f90",
    "derived_type_nested.f90",
    "allocatable.f90",
    "ar6_bss_module_data.f90",
];

fn scratch_dir(label: &str) -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "armfortas-benchmark-gate-{}-{id}-{label}",
        std::process::id()
    ));
    if dir.exists() {
        fs::remove_dir_all(&dir).expect("remove stale benchmark scratch tree");
    }
    fs::create_dir_all(dir.join("test_programs")).expect("create benchmark scratch tree");
    dir
}

fn script() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/benchmark_gate.sh")
}

fn prepare_gate(label: &str) -> (PathBuf, PathBuf) {
    let root = scratch_dir(label);
    for fixture in PROGRAMS {
        fs::write(
            root.join("test_programs").join(fixture),
            b"program p\nend\n",
        )
        .expect("write benchmark fixture");
    }

    let compiler = root.join("target/release/armfortas");
    fs::create_dir_all(compiler.parent().expect("compiler parent"))
        .expect("create fake compiler directory");
    fs::write(
        &compiler,
        r#"#!/bin/sh
if [ "${1:-}" = "--print-target" ]; then
    echo audit-test
    exit 0
fi
output=
while [ "$#" -gt 0 ]; do
    if [ "$1" = "-o" ]; then
        shift
        output=$1
    fi
    shift
done
if [ -z "$output" ]; then
    echo "missing -o" >&2
    exit 2
fi
printf 'fixed test binary\n' > "$output"
chmod +x "$output"
"#,
    )
    .expect("write fake compiler");
    fs::set_permissions(&compiler, fs::Permissions::from_mode(0o755))
        .expect("make fake compiler executable");

    let temp_base = root.join("temporary files");
    fs::create_dir(&temp_base).expect("create benchmark temporary base");
    (root, temp_base)
}

fn run_gate(root: &Path, temp_base: &Path, args: &[&str]) -> Output {
    Command::new("bash")
        .arg(script())
        .args(args)
        .env("TMPDIR", temp_base)
        .env("BENCH_SKIP_TIME", "1")
        .current_dir(root)
        .output()
        .expect("run benchmark gate")
}

fn assert_temp_base_empty(temp_base: &Path) {
    assert_eq!(
        fs::read_dir(temp_base)
            .expect("read benchmark temporary base")
            .count(),
        0,
        "benchmark temporary directory was not cleaned"
    );
}

#[test]
fn missing_fixed_fixture_fails_before_benchmarking() {
    let root = scratch_dir("missing-fixture");
    let missing = "two_loops.f90";
    for fixture in PROGRAMS {
        if *fixture != missing {
            fs::write(
                root.join("test_programs").join(fixture),
                b"program p\nend\n",
            )
            .expect("write benchmark fixture");
        }
    }

    let output = Command::new("bash")
        .arg(script())
        .current_dir(&root)
        .output()
        .expect("run benchmark gate");
    let _ = fs::remove_dir_all(&root);

    assert!(!output.status.success(), "missing fixture must fail");
    let stderr = String::from_utf8_lossy(&output.stderr);
    assert!(stderr.contains("missing mandatory benchmark fixture"));
    assert!(stderr.contains(missing));
}

#[test]
fn baseline_creation_is_explicit_and_comparison_requires_exact_identities() {
    let (root, temp_base) = prepare_gate("exact-baseline");
    let baseline = root.join(".benchmarks/baseline-audit-test.txt");

    let missing = run_gate(&root, &temp_base, &[]);
    assert!(
        !missing.status.success(),
        "missing baseline must fail closed"
    );
    assert!(
        String::from_utf8_lossy(&missing.stderr).contains("no benchmark baseline exists"),
        "unexpected missing-baseline diagnostic: {}",
        String::from_utf8_lossy(&missing.stderr)
    );
    assert!(!baseline.exists(), "comparison mode created a baseline");
    assert_temp_base_empty(&temp_base);

    let update = run_gate(&root, &temp_base, &["--update"]);
    assert!(
        update.status.success(),
        "explicit baseline update failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&update.stdout),
        String::from_utf8_lossy(&update.stderr)
    );
    assert_temp_base_empty(&temp_base);
    let complete = fs::read_to_string(&baseline).expect("read generated baseline");
    assert_eq!(complete.lines().count(), 5, "unexpected baseline row count");

    let compare = run_gate(&root, &temp_base, &[]);
    assert!(
        compare.status.success(),
        "exact baseline comparison failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compare.stdout),
        String::from_utf8_lossy(&compare.stderr)
    );
    assert_temp_base_empty(&temp_base);

    let missing_row = complete.lines().take(4).collect::<Vec<_>>().join("\n") + "\n";
    fs::write(&baseline, missing_row).expect("remove a baseline row");
    let missing_identity = run_gate(&root, &temp_base, &[]);
    assert!(
        !missing_identity.status.success(),
        "missing baseline identity passed"
    );
    assert!(String::from_utf8_lossy(&missing_identity.stderr)
        .contains("baseline identities do not exactly match"));
    assert_temp_base_empty(&temp_base);

    fs::write(&baseline, format!("{complete}stale_case 0.1000 18\n"))
        .expect("add a stale baseline row");
    let stale_identity = run_gate(&root, &temp_base, &[]);
    assert!(
        !stale_identity.status.success(),
        "stale baseline identity passed"
    );
    assert!(String::from_utf8_lossy(&stale_identity.stderr)
        .contains("baseline identities do not exactly match"));
    assert_temp_base_empty(&temp_base);

    fs::remove_dir_all(root).expect("remove benchmark scratch tree");
}
