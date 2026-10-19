use std::fs;
use std::os::unix::fs::PermissionsExt;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

const VERSION: &str = env!("CARGO_PKG_VERSION");
static NEXT_TEMP_ID: AtomicU64 = AtomicU64::new(0);

fn temp_dir(label: &str) -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!(
        "armfortas-release-test-{}-{id}-{label}",
        std::process::id()
    ));
    if path.exists() {
        fs::remove_dir_all(&path).expect("remove stale release test directory");
    }
    fs::create_dir_all(&path).expect("create release test directory");
    path
}

fn build_archive_with_tar(output_dir: &Path, tar_override: Option<(&Path, &Path)>) -> PathBuf {
    fs::create_dir_all(output_dir).expect("create archive output directory");
    let mut command = Command::new("bash");
    command
        .arg("scripts/package-release-source.sh")
        .arg(VERSION)
        .arg(output_dir)
        .current_dir(env!("CARGO_MANIFEST_DIR"));
    if let Some((tar, real_tar)) = tar_override {
        command.env("ARMFORTAS_TAR", tar);
        command.env("ARMFORTAS_TEST_REAL_TAR", real_tar);
    }
    let output = command.output().expect("run release source packager");
    assert!(
        output.status.success(),
        "source packager failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output_dir.join(format!("armfortas-{VERSION}.tar.gz"))
}

fn build_archive(output_dir: &Path) -> PathBuf {
    build_archive_with_tar(output_dir, None)
}

fn archive_entries(archive: &Path) -> Vec<String> {
    let listing = Command::new("tar")
        .args(["-tzf"])
        .arg(archive)
        .output()
        .expect("list release archive");
    assert!(
        listing.status.success(),
        "could not list release archive: {}",
        String::from_utf8_lossy(&listing.stderr)
    );
    std::str::from_utf8(&listing.stdout)
        .expect("archive paths are UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn git(repo: &Path, args: &[&str]) {
    let output = Command::new("git")
        .args(args)
        .current_dir(repo)
        .output()
        .expect("run git command");
    assert!(
        output.status.success(),
        "git {} failed in {}\nstdout:\n{}\nstderr:\n{}",
        args.join(" "),
        repo.display(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn archive_file(archive: &Path, path: &str) -> Vec<u8> {
    let output = Command::new("tar")
        .args(["-xOzf"])
        .arg(archive)
        .arg(path)
        .output()
        .expect("extract release archive member");
    assert!(
        output.status.success(),
        "could not extract {path}: {}",
        String::from_utf8_lossy(&output.stderr)
    );
    output.stdout
}

#[test]
fn complete_release_archive_is_deterministic_and_contains_submodules() {
    let gitmodules = fs::read_to_string(".gitmodules").expect("read .gitmodules");
    assert!(
        !gitmodules.contains("git@github.com:"),
        "public submodules must not require GitHub SSH credentials"
    );

    let temp = temp_dir("complete");
    let first = build_archive(&temp.join("first"));
    let second = build_archive(&temp.join("second"));
    let first_bytes = fs::read(&first).expect("read first archive");
    let second_bytes = fs::read(&second).expect("read second archive");
    assert_eq!(first_bytes, second_bytes, "release archives differ");

    let real_tar_output = Command::new("sh")
        .args(["-c", "command -v tar"])
        .output()
        .expect("locate the system tar");
    assert!(
        real_tar_output.status.success(),
        "could not locate the system tar"
    );
    let real_tar = PathBuf::from(
        std::str::from_utf8(&real_tar_output.stdout)
            .expect("system tar path is UTF-8")
            .trim(),
    );
    let busybox_shim = temp.join("busybox-tar");
    fs::write(
        &busybox_shim,
        r#"#!/bin/sh
if [ "$1" = "--version" ]; then
    echo 'tar (busybox) 1.37.0'
    exit 0
fi
for arg do
    if [ "$arg" = "--no-xattrs" ]; then
        echo 'BusyBox tar received --no-xattrs' >&2
        exit 64
    fi
done
exec "$ARMFORTAS_TEST_REAL_TAR" "$@"
"#,
    )
    .expect("write BusyBox tar shim");
    fs::set_permissions(&busybox_shim, fs::Permissions::from_mode(0o755))
        .expect("make BusyBox tar shim executable");
    let busybox = build_archive_with_tar(&temp.join("busybox"), Some((&busybox_shim, &real_tar)));

    let entries = archive_entries(&first);
    let busybox_entries = archive_entries(&busybox);
    let prefix = format!("armfortas-{VERSION}/");

    for required in [
        "Cargo.toml",
        "include/omp_lib.h",
        "runtime/Cargo.toml",
        "afs-as/Cargo.toml",
        "afs-ld/Cargo.toml",
        "bencch/bench/Cargo.toml",
    ] {
        let expected = format!("{prefix}{required}");
        assert!(
            entries.contains(&expected),
            "release archive is missing {expected}"
        );
        assert!(
            busybox_entries.contains(&expected),
            "BusyBox release archive is missing {expected}"
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

#[test]
fn release_archive_exports_committed_root_and_submodule_blobs() {
    let temp = temp_dir("committed-blobs");
    let child = temp.join("child-source");
    let parent = temp.join("parent");
    fs::create_dir_all(&child).expect("create child repository");
    fs::create_dir_all(&parent).expect("create parent repository");

    git(&child, &["init", "-q"]);
    fs::write(child.join("payload.txt"), "committed child\n").expect("write child payload");
    git(&child, &["add", "payload.txt"]);
    git(
        &child,
        &[
            "-c",
            "user.name=ARMFORTAS Test",
            "-c",
            "user.email=armfortas@example.invalid",
            "commit",
            "-qm",
            "child baseline",
        ],
    );

    git(&parent, &["init", "-q"]);
    fs::write(
        parent.join("Cargo.toml"),
        "[package]\nname = \"release-fixture\"\nversion = \"0.1.0\"\n",
    )
    .expect("write fixture manifest");
    fs::write(parent.join("payload.txt"), "committed root\n").expect("write root payload");
    let child_path = child.to_str().expect("UTF-8 child path");
    git(
        &parent,
        &[
            "-c",
            "protocol.file.allow=always",
            "submodule",
            "add",
            "-q",
            child_path,
            "vendor/child",
        ],
    );
    git(
        &parent,
        &[
            "add",
            "Cargo.toml",
            "payload.txt",
            ".gitmodules",
            "vendor/child",
        ],
    );
    git(
        &parent,
        &[
            "-c",
            "user.name=ARMFORTAS Test",
            "-c",
            "user.email=armfortas@example.invalid",
            "commit",
            "-qm",
            "parent baseline",
        ],
    );

    fs::write(parent.join("payload.txt"), "dirty root\n").expect("dirty root payload");
    git(&parent, &["add", "payload.txt"]);
    fs::write(parent.join("vendor/child/payload.txt"), "dirty child\n")
        .expect("dirty child payload");
    git(&parent.join("vendor/child"), &["add", "payload.txt"]);

    let output_dir = parent.join("dist");
    let packager =
        PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("scripts/package-release-source.sh");
    let output = Command::new(packager)
        .args(["0.1.0"])
        .arg(&output_dir)
        .current_dir(&parent)
        .output()
        .expect("run packager in fixture repository");
    assert!(
        output.status.success(),
        "fixture packaging failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );

    let archive = output_dir.join("armfortas-0.1.0.tar.gz");
    assert_eq!(
        archive_file(&archive, "armfortas-0.1.0/payload.txt"),
        b"committed root\n"
    );
    assert_eq!(
        archive_file(&archive, "armfortas-0.1.0/vendor/child/payload.txt"),
        b"committed child\n"
    );

    fs::remove_dir_all(temp).expect("remove release fixture repositories");
}
