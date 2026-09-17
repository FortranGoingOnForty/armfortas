use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

const VERSION: &str = env!("CARGO_PKG_VERSION");

fn temp_dir() -> PathBuf {
    let path = std::env::temp_dir().join(format!("armfortas-release-test-{}", std::process::id()));
    if path.exists() {
        fs::remove_dir_all(&path).expect("remove stale release test directory");
    }
    fs::create_dir_all(&path).expect("create release test directory");
    path
}

fn build_archive(output_dir: &Path) -> PathBuf {
    fs::create_dir_all(output_dir).expect("create archive output directory");
    let output = Command::new("bash")
        .arg("scripts/package-release-source.sh")
        .arg(VERSION)
        .arg(output_dir)
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .expect("run release source packager");
    assert!(
        output.status.success(),
        "source packager failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output_dir.join(format!("armfortas-{VERSION}.tar.gz"))
}

#[test]
fn complete_release_archive_is_deterministic_and_contains_submodules() {
    let gitmodules = fs::read_to_string(".gitmodules").expect("read .gitmodules");
    assert!(
        !gitmodules.contains("git@github.com:"),
        "public submodules must not require GitHub SSH credentials"
    );

    let temp = temp_dir();
    let first = build_archive(&temp.join("first"));
    let second = build_archive(&temp.join("second"));
    let first_bytes = fs::read(&first).expect("read first archive");
    let second_bytes = fs::read(&second).expect("read second archive");
    assert_eq!(first_bytes, second_bytes, "release archives differ");

    let listing = Command::new("tar")
        .args(["-tzf"])
        .arg(&first)
        .output()
        .expect("list release archive");
    assert!(
        listing.status.success(),
        "could not list release archive: {}",
        String::from_utf8_lossy(&listing.stderr)
    );
    let entries: Vec<&str> = std::str::from_utf8(&listing.stdout)
        .expect("archive paths are UTF-8")
        .lines()
        .collect();
    let prefix = format!("armfortas-{VERSION}/");

    for required in [
        "Cargo.toml",
        "runtime/Cargo.toml",
        "afs-as/Cargo.toml",
        "afs-ld/Cargo.toml",
        "bencch/bench/Cargo.toml",
    ] {
        let expected = format!("{prefix}{required}");
        assert!(
            entries.contains(&expected.as_str()),
            "release archive is missing {expected}"
        );
    }
    assert!(
        entries
            .iter()
            .all(|entry| entry.split('/').all(|component| component != ".git")),
        "release archive contains git metadata"
    );
    assert!(
        entries
            .iter()
            .all(|entry| entry == &format!("armfortas-{VERSION}") || entry.starts_with(&prefix)),
        "release archive contains a path outside its versioned root"
    );

    let checksum =
        fs::read_to_string(format!("{}.sha256", first.display())).expect("read archive checksum");
    assert!(
        checksum.ends_with(&format!("  armfortas-{VERSION}.tar.gz\n")),
        "checksum does not name the release archive: {checksum:?}"
    );

    fs::remove_dir_all(temp).expect("remove release test directory");
}
