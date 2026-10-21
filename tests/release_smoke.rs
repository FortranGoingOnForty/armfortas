use std::fs;
use std::path::{Path, PathBuf};
use std::process::Output;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn scratch_dir() -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "armfortas-release-smoke-{}-{id}",
        std::process::id()
    ));
    if path.exists() {
        fs::remove_dir_all(&path).expect("remove stale release smoke directory");
    }
    fs::create_dir(&path).expect("create release smoke directory");
    path
}

fn check_streams(root: &Path, stdout: &[u8], stderr: &[u8]) -> Output {
    let stdout_file = root.join("stdout.txt");
    let stderr_file = root.join("stderr.txt");
    fs::write(&stdout_file, stdout).expect("write smoke stdout");
    fs::write(&stderr_file, stderr).expect("write smoke stderr");
    std::process::Command::new("bash")
        .arg(Path::new(env!("CARGO_MANIFEST_DIR")).join("scripts/check-release-smoke-output.sh"))
        .arg(stdout_file)
        .arg(stderr_file)
        .output()
        .expect("run release smoke stream checker")
}

#[test]
fn release_smoke_requires_one_normalized_record_and_empty_stderr() {
    let root = scratch_dir();

    let valid = check_streams(&root, b"          42\n", b"");
    assert!(valid.status.success(), "normalized expected stream failed");

    for invalid in [
        b"142\n".as_slice(),
        b"42 trailing\n".as_slice(),
        b"42\nextra\n".as_slice(),
        b"42\n\n".as_slice(),
    ] {
        let output = check_streams(&root, invalid, b"");
        assert!(
            !output.status.success(),
            "unexpected stdout passed: {:?}",
            String::from_utf8_lossy(invalid)
        );
        assert!(String::from_utf8_lossy(&output.stderr).contains("unexpected normalized stdout"));
    }

    let noisy = check_streams(&root, b"42\n", b"runtime warning\n");
    assert!(!noisy.status.success(), "nonempty stderr passed");
    assert!(String::from_utf8_lossy(&noisy.stderr).contains("unexpected stderr"));

    fs::remove_dir_all(root).expect("remove release smoke directory");
}
