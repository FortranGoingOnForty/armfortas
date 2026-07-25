use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

fn compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary not built for this test profile")
}

fn unique_dir(stem: &str) -> PathBuf {
    let pid = std::process::id();
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("afs_{}_{}_{}.dir", stem, pid, id));
    std::fs::create_dir_all(&dir).expect("cannot create temp dir");
    dir
}

fn run_compile(compiler: &Path, dir: &Path) -> Output {
    Command::new(compiler)
        .current_dir(dir)
        .args(["p.f90", "-o", "t"])
        .output()
        .expect("compiler launch failed")
}

fn temporary_codegen_files(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .expect("cannot inspect temporary directory")
        .map(|entry| entry.expect("cannot read temporary entry").path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| {
                    name.starts_with("armfortas_") && (name.ends_with(".s") || name.ends_with(".o"))
                })
        })
        .collect()
}

#[test]
fn temporary_assembly_is_removed_after_success_and_failure() {
    let compiler = compiler();
    let dir = unique_dir("assembly_cleanup");
    let temp_dir = dir.join("tmp");
    std::fs::create_dir_all(&temp_dir).expect("cannot create temporary directory");
    let source = dir.join("p.f90");
    std::fs::write(&source, "program p\nend program\n").expect("cannot write source");

    let success = Command::new(&compiler)
        .env("TMPDIR", &temp_dir)
        .args(["-c"])
        .arg(&source)
        .arg("-o")
        .arg(dir.join("success.o"))
        .output()
        .expect("successful compiler launch failed");
    assert!(
        success.status.success(),
        "compile failed:\n{}",
        String::from_utf8_lossy(&success.stderr)
    );
    assert!(
        temporary_codegen_files(&temp_dir).is_empty(),
        "successful compile retained temporary assembly"
    );

    let failure = Command::new(&compiler)
        .env("TMPDIR", &temp_dir)
        .env("AFS_AS_PATH", "false")
        .args(["-c"])
        .arg(&source)
        .arg("-o")
        .arg(dir.join("failure.o"))
        .output()
        .expect("failing compiler launch failed");
    assert!(
        !failure.status.success(),
        "false assembler unexpectedly succeeded"
    );
    assert!(
        temporary_codegen_files(&temp_dir).is_empty(),
        "failed compile retained temporary codegen files: {:?}",
        temporary_codegen_files(&temp_dir)
    );

    if armfortas::testing::native_e2e_support().is_ok() {
        let link_failure = Command::new(&compiler)
            .env("TMPDIR", &temp_dir)
            .env("AFS_LD_PATH", "false")
            .arg(&source)
            .arg("-o")
            .arg(dir.join("failure"))
            .output()
            .expect("failing linker launch failed");
        assert!(
            !link_failure.status.success(),
            "false linker unexpectedly succeeded"
        );
        assert!(
            temporary_codegen_files(&temp_dir).is_empty(),
            "failed link retained temporary codegen files: {:?}",
            temporary_codegen_files(&temp_dir)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn zero_exit_external_tools_cannot_publish_missing_or_stale_outputs() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_temp_paths test=zero_exit_external_tools_cannot_publish_missing_or_stale_outputs count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let compiler = compiler();
    let dir = unique_dir("zero_exit_tool_outputs");
    let source = dir.join("p.f90");
    std::fs::write(&source, "program p\nend program\n").expect("cannot write source");

    for (phase, override_name, compile_only) in [
        ("assembler", "AFS_AS_PATH", true),
        ("linker", "AFS_LD_PATH", false),
    ] {
        for stale in [false, true] {
            let state = if stale { "stale" } else { "fresh" };
            let suffix = if compile_only { "o" } else { "bin" };
            let output = dir.join(format!("{phase}-{state}.{suffix}"));
            if stale {
                std::fs::write(&output, b"stale artifact")
                    .expect("cannot seed stale external-tool output");
            }

            let mut command = Command::new(&compiler);
            command
                .env(override_name, "true")
                .arg(&source)
                .arg("-o")
                .arg(&output);
            if compile_only {
                command.arg("-c");
            }
            let result = command
                .output()
                .unwrap_or_else(|error| panic!("{phase} {state} compiler launch failed: {error}"));
            assert!(
                !result.status.success(),
                "zero-exit {phase} unexpectedly published success for a {state} destination"
            );
            let stderr = String::from_utf8_lossy(&result.stderr);
            assert!(
                stderr.contains(&format!("{phase} reported success"))
                    && stderr.contains("did not produce"),
                "zero-exit {phase} {state} diagnostic did not identify the missing output:\n{stderr}"
            );
            assert!(
                !output.exists(),
                "zero-exit {phase} left a {state} destination looking current"
            );
        }
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn failing_external_tools_cannot_publish_partial_outputs() {
    use std::os::unix::fs::PermissionsExt;

    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_temp_paths test=failing_external_tools_cannot_publish_partial_outputs count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let compiler = compiler();
    let dir = unique_dir("partial_tool_outputs");
    let source = dir.join("p.f90");
    let tool = dir.join("partial-tool");
    std::fs::write(&source, "program p\nend program\n").expect("cannot write source");
    std::fs::write(
        &tool,
        "#!/bin/sh\n\
         while [ \"$#\" -gt 0 ]; do\n\
           if [ \"$1\" = \"-o\" ]; then\n\
             shift\n\
             printf 'partial artifact' > \"$1\"\n\
             exit 1\n\
           fi\n\
           shift\n\
         done\n\
         exit 2\n",
    )
    .expect("cannot write partial-output tool");
    let mut permissions = std::fs::metadata(&tool)
        .expect("cannot inspect partial-output tool")
        .permissions();
    permissions.set_mode(0o755);
    std::fs::set_permissions(&tool, permissions)
        .expect("cannot make partial-output tool executable");

    for (phase, override_name, compile_only) in [
        ("assembler", "AFS_AS_PATH", true),
        ("linker", "AFS_LD_PATH", false),
    ] {
        let suffix = if compile_only { "o" } else { "bin" };
        let output = dir.join(format!("{phase}.{suffix}"));
        std::fs::write(&output, b"stale artifact").expect("cannot seed stale external-tool output");

        let mut command = Command::new(&compiler);
        command
            .env(override_name, &tool)
            .arg(&source)
            .arg("-o")
            .arg(&output);
        if compile_only {
            command.arg("-c");
        }
        let result = command
            .output()
            .unwrap_or_else(|error| panic!("{phase} compiler launch failed: {error}"));
        assert!(
            !result.status.success(),
            "failing {phase} unexpectedly published success"
        );
        assert!(
            String::from_utf8_lossy(&result.stderr).contains(&format!("{phase} failed")),
            "failing {phase} diagnostic did not identify the phase:\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            !output.exists(),
            "failing {phase} left a partial destination looking current"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn external_tools_replace_stale_outputs_on_success() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_temp_paths test=external_tools_replace_stale_outputs_on_success count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let compiler = compiler();
    let assembler =
        armfortas::testing::built_binary("afs-as").expect("afs-as binary not built for test");
    let linker =
        armfortas::testing::built_binary("afs-ld").expect("afs-ld binary not built for test");
    let dir = unique_dir("external_tool_success");
    let source = dir.join("p.f90");
    std::fs::write(
        &source,
        "program p\n  print *, 'fresh external output'\nend program\n",
    )
    .expect("cannot write source");

    let object = dir.join("p.o");
    std::fs::write(&object, b"stale artifact").expect("cannot seed stale object");
    let assemble = Command::new(&compiler)
        .env("AFS_AS_PATH", &assembler)
        .args(["-c"])
        .arg(&source)
        .arg("-o")
        .arg(&object)
        .output()
        .expect("compiler launch failed");
    assert!(
        assemble.status.success(),
        "external assembler failed:\n{}",
        String::from_utf8_lossy(&assemble.stderr)
    );
    assert_ne!(
        std::fs::read(&object).expect("cannot read assembled object"),
        b"stale artifact",
        "external assembler retained the stale object"
    );

    let binary = dir.join("p");
    std::fs::write(&binary, b"stale artifact").expect("cannot seed stale binary");
    let link = Command::new(&compiler)
        .env("AFS_AS_PATH", &assembler)
        .env("AFS_LD_PATH", &linker)
        .arg(&source)
        .arg("-o")
        .arg(&binary)
        .output()
        .expect("compiler launch failed");
    assert!(
        link.status.success(),
        "external toolchain failed:\n{}",
        String::from_utf8_lossy(&link.stderr)
    );
    let run = Command::new(&binary)
        .output()
        .expect("cannot run linked binary");
    assert!(
        run.status.success(),
        "linked binary exited with {:?}:\n{}",
        run.status.code(),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("fresh external output"),
        "linked binary did not come from the fresh source:\n{}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[cfg(unix)]
#[test]
fn successful_external_tool_diagnostics_reach_compiler_stderr_in_phase_order() {
    use std::os::unix::fs::PermissionsExt;

    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_temp_paths test=successful_external_tool_diagnostics_reach_compiler_stderr_in_phase_order count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let compiler = compiler();
    let assembler =
        armfortas::testing::built_binary("afs-as").expect("afs-as binary not built for test");
    let linker =
        armfortas::testing::built_binary("afs-ld").expect("afs-ld binary not built for test");
    let dir = unique_dir("successful_tool_diagnostics");
    let source = dir.join("p.f90");
    std::fs::write(&source, "program p\nend program\n").expect("cannot write source");

    let assembler_wrapper = dir.join("assembler-warning.sh");
    std::fs::write(
        &assembler_wrapper,
        "#!/bin/sh\n\
         printf 'armfortas-test assembler warning\\377\\n' >&2\n\
         if [ \"${AR38_FORCE_FAILURE:-0}\" = 1 ]; then\n\
           exit 47\n\
         fi\n\
         exec \"$AR38_REAL_AS\" \"$@\"\n",
    )
    .expect("cannot write assembler wrapper");
    let linker_wrapper = dir.join("linker-warning.sh");
    std::fs::write(
        &linker_wrapper,
        "#!/bin/sh\n\
         printf 'armfortas-test linker warning\\376\\n' >&2\n\
         exec \"$AR38_REAL_LD\" \"$@\"\n",
    )
    .expect("cannot write linker wrapper");
    for wrapper in [&assembler_wrapper, &linker_wrapper] {
        let mut permissions = std::fs::metadata(wrapper)
            .expect("cannot inspect wrapper")
            .permissions();
        permissions.set_mode(0o755);
        std::fs::set_permissions(wrapper, permissions).expect("cannot make wrapper executable");
    }

    let object = dir.join("p.o");
    std::fs::write(&object, b"stale object").expect("cannot seed stale object");
    let assemble = Command::new(&compiler)
        .env("AFS_AS_PATH", &assembler_wrapper)
        .env("AR38_REAL_AS", &assembler)
        .args(["-O2", "-c"])
        .arg(&source)
        .arg("-o")
        .arg(&object)
        .output()
        .expect("compiler launch failed");
    assert!(
        assemble.status.success(),
        "successful diagnostic assembler failed:\n{}",
        String::from_utf8_lossy(&assemble.stderr)
    );
    assert_eq!(
        assemble.stderr, b"armfortas-test assembler warning\xff\n",
        "successful assembler diagnostics were lost or rewritten"
    );
    assert_ne!(
        std::fs::read(&object).expect("cannot read assembled object"),
        b"stale object",
        "successful diagnostic assembler retained stale output"
    );

    let failed_object = dir.join("failed.o");
    std::fs::write(&failed_object, b"stale failed object")
        .expect("cannot seed failed assembler output");
    let failed_assemble = Command::new(&compiler)
        .env("AFS_AS_PATH", &assembler_wrapper)
        .env("AR38_REAL_AS", &assembler)
        .env("AR38_FORCE_FAILURE", "1")
        .args(["-O2", "-c"])
        .arg(&source)
        .arg("-o")
        .arg(&failed_object)
        .output()
        .expect("failing compiler launch failed");
    assert!(
        !failed_assemble.status.success(),
        "forced assembler failure unexpectedly succeeded"
    );
    let failure_marker = b"armfortas-test assembler warning";
    assert_eq!(
        failed_assemble
            .stderr
            .windows(failure_marker.len())
            .filter(|window| *window == failure_marker)
            .count(),
        1,
        "failing assembler diagnostics were lost or duplicated:\n{}",
        String::from_utf8_lossy(&failed_assemble.stderr)
    );
    assert!(
        !failed_object.exists(),
        "failing diagnostic assembler retained a stale output"
    );

    for optimization in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        let binary = dir.join(format!("p-{}", &optimization[2..]));
        std::fs::write(&binary, b"stale executable").expect("cannot seed stale executable");
        let link = Command::new(&compiler)
            .env("AFS_AS_PATH", &assembler_wrapper)
            .env("AFS_LD_PATH", &linker_wrapper)
            .env("AR38_REAL_AS", &assembler)
            .env("AR38_REAL_LD", &linker)
            .arg(optimization)
            .arg(&source)
            .arg("-o")
            .arg(&binary)
            .output()
            .expect("compiler launch failed");
        assert!(
            link.status.success(),
            "successful diagnostic toolchain failed at {optimization}:\n{}",
            String::from_utf8_lossy(&link.stderr)
        );
        assert_eq!(
            link.stderr,
            b"armfortas-test assembler warning\xff\narmfortas-test linker warning\xfe\n",
            "successful tool diagnostics were lost, rewritten, or reordered at {optimization}"
        );
        let run = Command::new(&binary)
            .output()
            .expect("cannot run diagnostic-wrapper output");
        assert!(
            run.status.success(),
            "diagnostic-wrapper output at {optimization} exited with {:?}:\n{}",
            run.status.code(),
            String::from_utf8_lossy(&run.stderr)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn valid_long_output_basename_does_not_expand_temporary_name() {
    let compiler = compiler();
    let dir = unique_dir("long_output_basename");
    let temp_dir = dir.join("tmp");
    std::fs::create_dir_all(&temp_dir).expect("cannot create temporary directory");
    let source = dir.join("p.f90");
    std::fs::write(&source, "program p\nend program\n").expect("cannot write source");
    let output = dir.join(format!("{}.o", "x".repeat(230)));

    let result = Command::new(&compiler)
        .env("TMPDIR", &temp_dir)
        .args(["-c"])
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "valid long output basename failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(output.is_file(), "long-named object was not written");
    assert!(
        temporary_codegen_files(&temp_dir).is_empty(),
        "long output retained temporary codegen files"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn parallel_relative_outputs_with_same_basename_keep_separate_temps() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_temp_paths test=parallel_relative_outputs_with_same_basename_keep_separate_temps count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let compiler = compiler();
    let root = unique_dir("parallel_same_output_basename");
    let cases: Vec<(PathBuf, String)> = (0..8)
        .map(|i| {
            let dir = root.join(format!("case_{i}"));
            std::fs::create_dir_all(&dir).expect("cannot create case dir");
            let marker = format!("case_marker_{i}");
            std::fs::write(
                dir.join("p.f90"),
                format!("program p\n  print *, '{}'\nend program\n", marker),
            )
            .expect("cannot write test source");
            (dir, marker)
        })
        .collect();

    let handles: Vec<_> = cases
        .iter()
        .map(|(dir, _)| {
            let compiler = compiler.clone();
            let dir = dir.clone();
            std::thread::spawn(move || run_compile(&compiler, &dir))
        })
        .collect();

    for (idx, handle) in handles.into_iter().enumerate() {
        let output = handle.join().expect("compile thread panicked");
        assert!(
            output.status.success(),
            "compile {idx} failed:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    for (dir, marker) in &cases {
        let binary = dir.join("t");
        let output = Command::new(&binary)
            .output()
            .expect("compiled binary launch failed");
        assert!(
            output.status.success(),
            "{} exited with {:?}\nstderr: {}",
            binary.display(),
            output.status.code(),
            String::from_utf8_lossy(&output.stderr)
        );
        let stdout = String::from_utf8_lossy(&output.stdout);
        assert!(
            stdout.contains(marker),
            "{} printed wrong output:\n{}",
            binary.display(),
            stdout
        );
    }

    let _ = std::fs::remove_dir_all(&root);
}
