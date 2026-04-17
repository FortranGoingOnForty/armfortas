use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn unique_dir(prefix: &str) -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "armfortas_{}_{}_{}",
        prefix,
        std::process::id(),
        id
    ));
    fs::create_dir_all(&dir).expect("create temp dir");
    dir
}

fn workspace_root() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR"))
}

fn binary(name: &str) -> Option<PathBuf> {
    if let Some(path) = std::env::var_os(format!("CARGO_BIN_EXE_{name}")) {
        return Some(PathBuf::from(path));
    }
    let root = workspace_root();
    for candidate in [
        root.join("target/debug").join(name),
        root.join("target/release").join(name),
    ] {
        if candidate.exists() {
            return Some(fs::canonicalize(candidate).expect("canonicalize sibling binary path"));
        }
    }
    None
}

fn runtime_archive() -> Option<PathBuf> {
    let root = workspace_root();
    for candidate in [
        root.join("target/debug/libarmfortas_rt.a"),
        root.join("target/release/libarmfortas_rt.a"),
    ] {
        if candidate.exists() {
            return Some(candidate);
        }
    }
    None
}

fn libsystem_tbd() -> Option<PathBuf> {
    let output = Command::new("xcrun")
        .args(["--sdk", "macosx", "--show-sdk-path"])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let sdk = String::from_utf8(output.stdout).ok()?;
    let path = PathBuf::from(sdk.trim()).join("usr/lib/libSystem.tbd");
    path.exists().then_some(path)
}

fn run_command(cmd: &mut Command, context: &str) -> Output {
    cmd.output()
        .unwrap_or_else(|err| panic!("{context}: failed to spawn: {err}"))
}

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn run_binary(path: &Path, context: &str) -> Output {
    let output = Command::new(path)
        .output()
        .unwrap_or_else(|err| panic!("{context}: failed to launch {}: {err}", path.display()));
    assert!(
        output.status.success(),
        "{context} exited with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
    output
}

#[test]
fn hello_world_runs_through_afs_as_and_afs_ld() {
    let Some(armfortas) = binary("armfortas") else {
        eprintln!("skipping: armfortas binary not built");
        return;
    };
    let Some(afs_as) = binary("afs-as") else {
        eprintln!("skipping: afs-as binary not built");
        return;
    };
    let Some(afs_ld) = binary("afs-ld") else {
        eprintln!("skipping: afs-ld binary not built");
        return;
    };
    let Some(runtime) = runtime_archive() else {
        eprintln!("skipping: libarmfortas_rt.a not built");
        return;
    };
    let Some(libsystem) = libsystem_tbd() else {
        eprintln!("skipping: libSystem.tbd not found");
        return;
    };

    let source = workspace_root().join("test_programs/hello.f90");
    assert!(source.exists(), "hello.f90 missing at {}", source.display());

    let dir = unique_dir("standalone_hello");
    let default_bin = dir.join("hello-default");
    let asm = dir.join("hello.s");
    let obj = dir.join("hello.o");
    let standalone_bin = dir.join("hello-standalone");

    let default_compile = run_command(
        Command::new(&armfortas)
            .arg(&source)
            .arg("-o")
            .arg(&default_bin),
        "default armfortas compile",
    );
    assert_success(&default_compile, "default armfortas compile");
    let default_run = run_binary(&default_bin, "default armfortas run");

    let asm_compile = run_command(
        Command::new(&armfortas)
            .arg("-S")
            .arg(&source)
            .arg("-o")
            .arg(&asm),
        "armfortas -S compile",
    );
    assert_success(&asm_compile, "armfortas -S compile");
    assert!(
        asm.exists(),
        "missing emitted assembly at {}",
        asm.display()
    );

    let assemble = run_command(
        Command::new(&afs_as).arg(&asm).arg("-o").arg(&obj),
        "afs-as assemble",
    );
    assert_success(&assemble, "afs-as assemble");

    let link = run_command(
        Command::new(&afs_ld)
            .arg("-arch")
            .arg("arm64")
            .arg("-e")
            .arg("_main")
            .arg("-o")
            .arg(&standalone_bin)
            .arg(&obj)
            .arg(&runtime)
            .arg(&libsystem),
        "afs-ld link",
    );
    assert_success(&link, "afs-ld link");

    let standalone_run = run_binary(&standalone_bin, "standalone toolchain run");

    let default_stdout = String::from_utf8_lossy(&default_run.stdout);
    let standalone_stdout = String::from_utf8_lossy(&standalone_run.stdout);
    let default_stderr = String::from_utf8_lossy(&default_run.stderr);
    let standalone_stderr = String::from_utf8_lossy(&standalone_run.stderr);

    assert_eq!(
        standalone_stdout, default_stdout,
        "standalone stdout diverged from default driver"
    );
    assert_eq!(
        standalone_stderr, default_stderr,
        "standalone stderr diverged from default driver"
    );
    assert_eq!(standalone_stdout, " Hello, World!\n");
}
