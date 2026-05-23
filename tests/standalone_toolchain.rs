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

fn file_description(path: &Path, context: &str) -> String {
    let output = run_command(Command::new("file").arg(path), context);
    assert_success(&output, context);
    String::from_utf8(output.stdout).expect("file output is utf-8")
}

fn compile_with_driver(
    driver: &Path,
    source: &Path,
    output: &Path,
    extra_envs: &[(&str, &Path)],
    context: &str,
) -> Output {
    compile_with_driver_args(driver, source, output, extra_envs, &[], context)
}

fn compile_with_driver_args(
    driver: &Path,
    source: &Path,
    output: &Path,
    extra_envs: &[(&str, &Path)],
    extra_args: &[&str],
    context: &str,
) -> Output {
    compile_with_driver_args_and_vars(driver, source, output, extra_envs, &[], extra_args, context)
}

fn compile_with_driver_args_and_vars(
    driver: &Path,
    source: &Path,
    output: &Path,
    extra_envs: &[(&str, &Path)],
    extra_vars: &[(&str, &str)],
    extra_args: &[&str],
    context: &str,
) -> Output {
    let mut cmd = Command::new(driver);
    for (name, value) in extra_envs {
        cmd.env(name, value);
    }
    for (name, value) in extra_vars {
        cmd.env(name, value);
    }
    cmd.arg(source).arg("-o").arg(output).args(extra_args);
    run_command(&mut cmd, context)
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

#[test]
fn hello_world_runs_through_driver_with_standalone_tool_overrides() {
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

    let dir = unique_dir("driver_standalone_hello");
    let default_bin = dir.join("hello-default");
    let standalone_bin = dir.join("hello-driver-standalone");

    let default_compile = compile_with_driver(
        &armfortas,
        &source,
        &default_bin,
        &[],
        "default armfortas compile",
    );
    assert_success(&default_compile, "default armfortas compile");
    let default_run = run_binary(&default_bin, "default armfortas run");

    let standalone_compile = compile_with_driver(
        &armfortas,
        &source,
        &standalone_bin,
        &[
            ("AFS_AS_PATH", &afs_as),
            ("AFS_LD_PATH", &afs_ld),
            ("AFS_RUNTIME_PATH", &runtime),
            ("AFS_LIBSYSTEM_TBD", &libsystem),
        ],
        "standalone armfortas compile",
    );
    assert_success(&standalone_compile, "standalone armfortas compile");
    let standalone_run = run_binary(&standalone_bin, "standalone armfortas run");

    let default_stdout = String::from_utf8_lossy(&default_run.stdout);
    let standalone_stdout = String::from_utf8_lossy(&standalone_run.stdout);
    let default_stderr = String::from_utf8_lossy(&default_run.stderr);
    let standalone_stderr = String::from_utf8_lossy(&standalone_run.stderr);

    assert_eq!(
        standalone_stdout, default_stdout,
        "driver override stdout diverged from default driver"
    );
    assert_eq!(
        standalone_stderr, default_stderr,
        "driver override stderr diverged from default driver"
    );
    assert_eq!(standalone_stdout, " Hello, World!\n");
}

#[test]
fn hello_world_runs_through_driver_with_afs_ld_enable_flag() {
    let Some(armfortas) = binary("armfortas") else {
        eprintln!("skipping: armfortas binary not built");
        return;
    };
    let Some(afs_as) = binary("afs-as") else {
        eprintln!("skipping: afs-as binary not built");
        return;
    };
    let Some(_afs_ld) = binary("afs-ld") else {
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

    let dir = unique_dir("driver_afs_ld_flag_hello");
    let standalone_bin = dir.join("hello-driver-afs-ld-flag");

    let standalone_compile = compile_with_driver_args_and_vars(
        &armfortas,
        &source,
        &standalone_bin,
        &[
            ("AFS_AS_PATH", &afs_as),
            ("AFS_RUNTIME_PATH", &runtime),
            ("AFS_LIBSYSTEM_TBD", &libsystem),
        ],
        &[("AFS_LD", "1")],
        &["-L", "/tmp", "-rpath", "/tmp"],
        "AFS_LD=1 armfortas compile",
    );
    assert_success(&standalone_compile, "AFS_LD=1 armfortas compile");
    let standalone_run = run_binary(&standalone_bin, "AFS_LD=1 armfortas run");
    let standalone_stdout = String::from_utf8_lossy(&standalone_run.stdout);
    assert_eq!(standalone_stdout, " Hello, World!\n");
}

#[test]
fn shared_library_runs_through_driver_with_standalone_linker_override() {
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

    let dir = unique_dir("driver_standalone_shared");
    let lib_src = dir.join("mylib.f90");
    let user_src = dir.join("user.f90");
    fs::write(
        &lib_src,
        "module m\ncontains\n  integer function answer()\n    answer = 42\n  end function\nend module\n",
    )
    .expect("write shared library source");
    fs::write(
        &user_src,
        "program p\n  use m\n  print *, answer()\nend program\n",
    )
    .expect("write shared library consumer source");

    let dylib = dir.join("libmylib.dylib");
    let shared_compile = run_command(
        Command::new(&armfortas)
            .env("AFS_AS_PATH", &afs_as)
            .env("AFS_LD_PATH", &afs_ld)
            .env("AFS_RUNTIME_PATH", &runtime)
            .env("AFS_LIBSYSTEM_TBD", &libsystem)
            .arg("-shared")
            .arg(&lib_src)
            .arg("-o")
            .arg(&dylib),
        "standalone armfortas shared compile",
    );
    assert_success(&shared_compile, "standalone armfortas shared compile");
    assert!(
        dir.join("m.amod").exists(),
        "standalone shared compile should emit module interface"
    );

    let exe = dir.join("use_m");
    let dir_str = dir.to_str().expect("temp dir is utf-8");
    let user_compile = run_command(
        Command::new(&armfortas)
            .env("AFS_AS_PATH", &afs_as)
            .env("AFS_LD_PATH", &afs_ld)
            .env("AFS_RUNTIME_PATH", &runtime)
            .env("AFS_LIBSYSTEM_TBD", &libsystem)
            .arg("-I")
            .arg(dir_str)
            .arg("-L")
            .arg(dir_str)
            .arg("-rpath")
            .arg(dir_str)
            .arg("-lmylib")
            .arg(&user_src)
            .arg("-o")
            .arg(&exe),
        "standalone armfortas shared consumer compile",
    );
    assert_success(
        &user_compile,
        "standalone armfortas shared consumer compile",
    );

    let run = run_binary(&exe, "standalone armfortas shared consumer run");
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("42"),
        "shared consumer output should contain 42: {}",
        String::from_utf8_lossy(&run.stdout)
    );
}

#[test]
fn hello_world_compiles_to_object_with_standalone_assembler_override() {
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

    let source = workspace_root().join("test_programs/hello.f90");
    assert!(source.exists(), "hello.f90 missing at {}", source.display());

    let dir = unique_dir("driver_standalone_hello_obj");
    let default_obj = dir.join("hello-default.o");
    let standalone_obj = dir.join("hello-standalone.o");

    let default_compile = run_command(
        Command::new(&armfortas)
            .arg("-c")
            .arg(&source)
            .arg("-o")
            .arg(&default_obj),
        "default armfortas -c",
    );
    assert_success(&default_compile, "default armfortas -c");

    let standalone_compile = run_command(
        Command::new(&armfortas)
            .env("AFS_AS_PATH", &afs_as)
            .env("AFS_LD_PATH", &afs_ld)
            .arg("-c")
            .arg(&source)
            .arg("-o")
            .arg(&standalone_obj),
        "standalone armfortas -c",
    );
    assert_success(&standalone_compile, "standalone armfortas -c");

    let default_file = file_description(&default_obj, "file default hello object");
    let standalone_file = file_description(&standalone_obj, "file standalone hello object");

    assert!(
        default_file.contains("Mach-O 64-bit object arm64"),
        "unexpected default object shape: {default_file}"
    );
    assert!(
        standalone_file.contains("Mach-O 64-bit object arm64"),
        "unexpected standalone object shape: {standalone_file}"
    );
}

#[test]
fn sprint18_program_matrix_runs_through_driver_standalone_overrides() {
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

    let root = workspace_root();
    let cases = [
        ("arithmetic.f90", "30"),
        ("if_else.f90", "positive"),
        ("negative_step.f90", "5"),
        ("real_function.f90", "6.28"),
    ];

    for (name, needle) in cases {
        let source = root.join("test_programs").join(name);
        assert!(
            source.exists(),
            "missing source fixture {}",
            source.display()
        );

        let dir = unique_dir(&format!("standalone_matrix_{name}"));
        let default_bin = dir.join(format!("{name}.default.out"));
        let standalone_bin = dir.join(format!("{name}.standalone.out"));

        let default_compile = compile_with_driver(
            &armfortas,
            &source,
            &default_bin,
            &[],
            &format!("default armfortas compile for {name}"),
        );
        assert_success(
            &default_compile,
            &format!("default armfortas compile for {name}"),
        );
        let default_run = run_binary(&default_bin, &format!("default armfortas run for {name}"));

        let standalone_compile = compile_with_driver(
            &armfortas,
            &source,
            &standalone_bin,
            &[
                ("AFS_AS_PATH", &afs_as),
                ("AFS_LD_PATH", &afs_ld),
                ("AFS_RUNTIME_PATH", &runtime),
                ("AFS_LIBSYSTEM_TBD", &libsystem),
            ],
            &format!("standalone armfortas compile for {name}"),
        );
        assert_success(
            &standalone_compile,
            &format!("standalone armfortas compile for {name}"),
        );
        let standalone_run = run_binary(
            &standalone_bin,
            &format!("standalone armfortas run for {name}"),
        );

        let default_stdout = String::from_utf8_lossy(&default_run.stdout);
        let standalone_stdout = String::from_utf8_lossy(&standalone_run.stdout);
        let default_stderr = String::from_utf8_lossy(&default_run.stderr);
        let standalone_stderr = String::from_utf8_lossy(&standalone_run.stderr);

        assert_eq!(
            standalone_stdout, default_stdout,
            "driver override stdout diverged from default driver for {name}"
        );
        assert_eq!(
            standalone_stderr, default_stderr,
            "driver override stderr diverged from default driver for {name}"
        );
        assert!(
            standalone_stdout.contains(needle),
            "driver override output for {name} missing '{needle}': {}",
            standalone_stdout
        );
    }
}
