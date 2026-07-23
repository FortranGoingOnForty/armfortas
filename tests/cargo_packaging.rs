use std::path::Path;
use std::process::Command;

fn package_files(package: &str) -> Vec<String> {
    let output = Command::new(env!("CARGO"))
        .args([
            "package",
            "--package",
            package,
            "--offline",
            "--locked",
            "--allow-dirty",
            "--list",
        ])
        .current_dir(env!("CARGO_MANIFEST_DIR"))
        .output()
        .unwrap_or_else(|err| panic!("could not run cargo package for {package}: {err}"));

    assert!(
        output.status.success(),
        "cargo package --list failed for {package}\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8(output.stdout)
        .expect("Cargo package paths must be UTF-8")
        .lines()
        .map(str::to_owned)
        .collect()
}

fn assert_contains(files: &[String], expected: &str) {
    assert!(
        files.iter().any(|path| path == expected),
        "package is missing {expected}"
    );
}

#[test]
fn compiler_package_has_a_narrow_registry_ready_boundary() {
    let files = package_files("armfortas");

    for expected in [
        "Cargo.toml",
        "LICENSE",
        "README.md",
        "src/bin/afs.rs",
        "src/lib.rs",
        "src/main.rs",
    ] {
        assert_contains(&files, expected);
    }

    for forbidden in [
        ".docs/",
        "afs-as/",
        "afs-ld/",
        "bencch/",
        "runtime/",
        "test_programs/",
        "tests/",
    ] {
        assert!(
            files.iter().all(|path| !path.starts_with(forbidden)),
            "compiler package unexpectedly contains {forbidden}"
        );
    }
    assert!(
        files.iter().all(|path| path != "build.rs"),
        "compiler package must obtain its runtime archive from armfortas-rt"
    );
    assert!(
        files.len() < 200,
        "compiler package boundary unexpectedly grew to {} files",
        files.len()
    );
}

#[test]
fn runtime_dependency_package_carries_its_archive_builder() {
    let files = package_files("armfortas-rt");

    for expected in [
        "Cargo.toml",
        "build.rs",
        "src/array.rs",
        "src/bundle.rs",
        "src/lib.rs",
    ] {
        assert_contains(&files, expected);
    }
    assert!(
        files.iter().all(|path| !Path::new(path).is_absolute()),
        "runtime package emitted an absolute path"
    );
}
