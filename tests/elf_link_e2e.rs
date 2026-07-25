//! Sprint x06 end-state gate: `armfortas foo.f90 -o foo && ./foo` on
//! an x86_64 ELF host. The driver links by invoking `ld` directly (no
//! cc anywhere); these tests run the produced binaries and compare
//! against the programs' CHECK lines, pin the PT_INTERP path, and
//! cover the CLI diagnostics for the link paths that are out of scope
//! this sprint. Skips with a count elsewhere (x01 convention).

use std::path::{Path, PathBuf};
use std::process::Command;

use armfortas::target::{Arch, Libc, ObjectFormat, Os, TargetSpec};

const PROGRAMS: &[&str] = &[
    "hello",
    "x05_int_loops",
    "x05_fp_compare",
    "x05_if_chains",
    "x05_mod_div",
    "x05_conversions",
    "x05_big_frame",
];

fn compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary not built for this test profile")
}

fn afs_ld() -> PathBuf {
    armfortas::testing::built_binary("afs-ld")
        .expect("afs-ld binary not built for this test profile")
}

fn programs_dir() -> PathBuf {
    for dir in ["test_programs", "../test_programs"] {
        if Path::new(dir).exists() {
            return PathBuf::from(dir);
        }
    }
    panic!("cannot find test_programs/");
}

/// x86_64 ELF host whose libc the link step supports this sprint —
/// musl hosts (Alpine CI) wait for x11 — and whose crt objects are
/// discoverable. The crt probe mirrors the driver: on layouts without
/// an FHS crt location (NixOS) these tests need AFS_CRT_DIR (and
/// LIBRARY_PATH for libgcc_s) in the environment, which the spawned
/// compiler inherits.
fn host_can_link() -> bool {
    let host = TargetSpec::host();
    if host.arch != Arch::X86_64
        || host.object_format() != ObjectFormat::Elf
        || host.libc == Libc::Musl
    {
        return false;
    }
    let override_dirs: Vec<PathBuf> = std::env::var("AFS_CRT_DIR")
        .map(|v| {
            v.split(':')
                .filter(|d| !d.is_empty())
                .map(PathBuf::from)
                .collect()
        })
        .unwrap_or_default();
    armfortas::driver::elf_crt::find_crt(&host, &override_dirs, true).is_ok()
}

fn skip(test: &str, count: usize) -> bool {
    if host_can_link() {
        return false;
    }
    armfortas::testing::report_harness_skip(
        "elf_link_e2e",
        test,
        count,
        "needs an x86_64 ELF glibc or FreeBSD host with discoverable crt objects (musl: x11; NixOS: set AFS_CRT_DIR and LIBRARY_PATH)",
    );
    true
}

fn host_can_link_without_overrides() -> bool {
    let host = TargetSpec::host();
    if host.arch != Arch::X86_64
        || host.object_format() != ObjectFormat::Elf
        || host.libc == Libc::Musl
    {
        return false;
    }
    armfortas::driver::elf_crt::find_crt(&host, &[], false).is_ok()
}

fn build(program: &str, tag: &str, extra: &[&str]) -> PathBuf {
    let src = programs_dir().join(format!("{}.f90", program));
    // Tag disambiguates tests that build the same program in the same
    // process (parallel test threads would race on one path).
    let out = std::env::temp_dir().join(format!(
        "afs_elfe2e_{}_{}_{}",
        program,
        tag,
        std::process::id()
    ));
    let r = Command::new(compiler())
        .args(extra)
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .output()
        .expect("cannot run armfortas");
    assert!(
        r.status.success(),
        "{} failed to build:\n{}",
        program,
        String::from_utf8_lossy(&r.stderr)
    );
    out
}

fn check_lines(program: &str) -> Vec<String> {
    let src = programs_dir().join(format!("{}.f90", program));
    std::fs::read_to_string(&src)
        .unwrap()
        .lines()
        .filter_map(|l| {
            l.trim()
                .strip_prefix("! CHECK:")
                .map(|v| v.trim().to_string())
        })
        .collect()
}

#[test]
fn curated_programs_link_run_and_match_check_lines() {
    if skip(
        "curated_programs_link_run_and_match_check_lines",
        PROGRAMS.len(),
    ) {
        return;
    }
    for program in PROGRAMS {
        let bin = build(program, "curated", &[]);
        let r = Command::new(&bin).output().expect("cannot run binary");
        assert!(
            r.status.success(),
            "{} exited nonzero ({:?}):\nstdout: {}\nstderr: {}",
            program,
            r.status.code(),
            String::from_utf8_lossy(&r.stdout),
            String::from_utf8_lossy(&r.stderr)
        );
        let stdout = String::from_utf8_lossy(&r.stdout);
        let got: Vec<&str> = stdout.lines().map(str::trim).collect();
        let want = check_lines(program);
        assert_eq!(
            got, want,
            "{} output mismatch:\nwant {:?}\ngot  {:?}",
            program, want, got
        );
        let _ = std::fs::remove_file(&bin);
    }
}

#[test]
fn binaries_request_the_target_dynamic_linker() {
    if skip("binaries_request_the_target_dynamic_linker", 1) {
        return;
    }
    let want = match TargetSpec::host().os {
        Os::FreeBsd => "/libexec/ld-elf.so.1",
        Os::Linux => "/lib64/ld-linux-x86-64.so.2",
        Os::MacOs => unreachable!(),
    };
    let bin = build("hello", "interp", &[]);
    let readelf =
        armfortas::testing::find_inspection_tool("AFS_READELF_BIN", &["llvm-readelf", "readelf"]);
    let r = Command::new(&readelf)
        .arg("-l")
        .arg(&bin)
        .output()
        .expect("cannot run readelf");
    let text = String::from_utf8_lossy(&r.stdout);
    assert!(
        text.contains(want),
        "PT_INTERP should request {}:\n{}",
        want,
        text
    );
    let _ = std::fs::remove_file(&bin);
}

#[test]
fn no_pie_links_a_position_dependent_executable_that_runs() {
    if skip("no_pie_links_a_position_dependent_executable_that_runs", 1) {
        return;
    }
    let bin = build("hello", "nopie", &["-no-pie"]);
    let r = Command::new(&bin).output().expect("cannot run binary");
    assert!(r.status.success(), "-no-pie hello exited nonzero");
    assert!(String::from_utf8_lossy(&r.stdout).contains("Hello, World!"));
    let readelf =
        armfortas::testing::find_inspection_tool("AFS_READELF_BIN", &["llvm-readelf", "readelf"]);
    let hdr = Command::new(&readelf)
        .arg("-h")
        .arg(&bin)
        .output()
        .expect("cannot run readelf");
    let text = String::from_utf8_lossy(&hdr.stdout);
    assert!(
        text.contains("EXEC"),
        "-no-pie should produce ET_EXEC, got:\n{}",
        text
    );
    let _ = std::fs::remove_file(&bin);
}

#[test]
fn afs_ld_route_links_without_explicit_crt_dir() {
    if !host_can_link_without_overrides() {
        eprintln!("\nHARNESS_SKIP suite=elf_link_e2e test=afs_ld_route_links_without_explicit_crt_dir count=1 reason=\"needs an x86_64 ELF glibc or FreeBSD host with built-in crt discovery\"");
        return;
    }
    let afs_ld = afs_ld();
    let src = programs_dir().join("hello.f90");
    let readelf =
        armfortas::testing::find_inspection_tool("AFS_READELF_BIN", &["llvm-readelf", "readelf"]);
    let afs_ld_path = afs_ld.to_string_lossy().into_owned();
    for (tag, env_name, env_value) in [
        ("path", "AFS_LD_PATH", afs_ld_path.as_str()),
        ("flag", "AFS_LD", "1"),
    ] {
        let out =
            std::env::temp_dir().join(format!("afs_elfe2e_afsld_{}_{}", tag, std::process::id()));
        let r = Command::new(compiler())
            .env_remove("AFS_CRT_DIR")
            .env_remove("AFS_LD")
            .env_remove("AFS_LD_PATH")
            .env(env_name, env_value)
            .arg(&src)
            .arg("-o")
            .arg(&out)
            .output()
            .expect("cannot run armfortas");
        assert!(
            r.status.success(),
            "{}={} failed:\n{}",
            env_name,
            env_value,
            String::from_utf8_lossy(&r.stderr)
        );
        let run = Command::new(&out).output().expect("cannot run binary");
        assert!(run.status.success(), "{env_name} binary exited nonzero");
        assert!(String::from_utf8_lossy(&run.stdout).contains("Hello, World!"));

        let hdr = Command::new(&readelf)
            .arg("-h")
            .arg(&out)
            .output()
            .expect("cannot run readelf");
        let text = String::from_utf8_lossy(&hdr.stdout);
        assert!(
            text.contains("EXEC"),
            "afs-ld ELF route should use non-PIE ET_EXEC until PIE lands:\n{}",
            text
        );
        let _ = std::fs::remove_file(&out);
    }
}

/// Out-of-scope link paths fail with actionable diagnostics instead of
/// entering the linker with an unsupported configuration.
#[test]
fn out_of_scope_link_paths_diagnose_cleanly() {
    if skip("out_of_scope_link_paths_diagnose_cleanly", 3) {
        return;
    }
    let src = programs_dir().join("hello.f90");
    let out = std::env::temp_dir().join(format!("afs_elfe2e_diag_{}", std::process::id()));

    // AFS_LD routing is honored on ELF now: a missing substitute linker
    // must be reported as a tool lookup failure, not hidden behind an
    // ELF path guard.
    let r = Command::new(compiler())
        .env("AFS_LD_PATH", "/nonexistent/afs-ld")
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(!r.status.success());
    assert!(
        String::from_utf8_lossy(&r.stderr).contains("cannot run linker"),
        "a missing substitute linker must be named in the diagnostic, got:\n{}",
        String::from_utf8_lossy(&r.stderr)
    );

    let r = Command::new(compiler())
        .args(["-static"])
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(!r.status.success());
    assert!(
        String::from_utf8_lossy(&r.stderr).contains("static CRT/runtime discovery"),
        "-static should explain the missing ELF capability"
    );

    let r = Command::new(compiler())
        .args(["--target", "x86_64-linux-musl"])
        .arg(&src)
        .arg("-o")
        .arg(&out)
        .output()
        .unwrap();
    assert!(!r.status.success(), "musl link should be rejected for now");
    let _ = std::fs::remove_file(&out);
}

#[test]
fn unsupported_elf_link_modes_fail_before_compilation_and_discard_owned_output() {
    if TargetSpec::host().object_format() != ObjectFormat::Elf {
        armfortas::testing::report_harness_skip(
            "elf_link_e2e",
            "unsupported_elf_link_modes_fail_before_compilation_and_discard_owned_output",
            1,
            "needs an ELF host",
        );
        return;
    }

    let dir = std::env::temp_dir().join(format!("afs_elf_link_modes_{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let source = dir.join("must_not_compile.f90");
    std::fs::write(
        &source,
        "#error compilation pipeline ran\nprogram p\nend program p\n",
    )
    .unwrap();

    for (flag, expected) in [
        (
            "-shared",
            "shared-library linking is currently available only for Mach-O targets",
        ),
        ("-static", "static CRT/runtime discovery is not implemented"),
    ] {
        for stale in [false, true] {
            let state = if stale { "stale" } else { "fresh" };
            let output = dir.join(format!("{}_{state}.out", &flag[1..]));
            if stale {
                std::fs::write(&output, b"stale link output").unwrap();
            }

            let result = Command::new(compiler())
                .arg(flag)
                .arg(&source)
                .arg("-o")
                .arg(&output)
                .env("NO_COLOR", "1")
                .output()
                .expect("cannot run armfortas");
            assert_eq!(
                result.status.code(),
                Some(1),
                "{flag} must fail cleanly on ELF with {state} output"
            );
            let stderr = String::from_utf8_lossy(&result.stderr);
            assert!(
                stderr.contains(expected) && !stderr.contains("compilation pipeline ran"),
                "{flag} did not fail before compilation with {state} output:\n{stderr}"
            );
            assert!(
                !output.exists(),
                "{flag} retained {state} output after the rejected ELF link"
            );
        }
    }

    let compile_only_source = dir.join("compile_only.f90");
    std::fs::write(
        &compile_only_source,
        "subroutine compile_only\nend subroutine compile_only\n",
    )
    .unwrap();
    for flag in ["-shared", "-static"] {
        let object = dir.join(format!("{}_compile_only.o", &flag[1..]));
        let result = Command::new(compiler())
            .args([flag, "-c"])
            .arg(&compile_only_source)
            .arg("-o")
            .arg(&object)
            .env("NO_COLOR", "1")
            .output()
            .expect("cannot run armfortas");
        assert!(
            result.status.success() && object.is_file(),
            "{flag} was incorrectly rejected in compile-only mode:\n{}",
            String::from_utf8_lossy(&result.stderr)
        );
    }

    let aliased_source = dir.join("aliased.f90");
    let aliased_bytes = b"program aliased\nend program aliased\n";
    std::fs::write(&aliased_source, aliased_bytes).unwrap();
    let alias_result = Command::new(compiler())
        .arg("-shared")
        .arg(&aliased_source)
        .arg("-o")
        .arg(&aliased_source)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    assert_eq!(alias_result.status.code(), Some(1));
    assert_eq!(
        std::fs::read(&aliased_source).unwrap(),
        aliased_bytes,
        "unsupported-mode cleanup deleted an aliased compiler input"
    );
    assert!(
        String::from_utf8_lossy(&alias_result.stderr)
            .contains("shared-library linking is currently available only for Mach-O"),
        "aliased-output rejection lost the unsupported-mode diagnostic"
    );

    let second_source = dir.join("second.f90");
    std::fs::write(
        &second_source,
        "#error multi-input compilation pipeline ran\nprogram q\nend program q\n",
    )
    .unwrap();
    let multi_output = dir.join("multi.out");
    std::fs::write(&multi_output, b"stale multi-input link output").unwrap();
    let multi_result = Command::new(compiler())
        .arg("-shared")
        .arg(&source)
        .arg(&second_source)
        .arg("-o")
        .arg(&multi_output)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    let multi_stderr = String::from_utf8_lossy(&multi_result.stderr);
    assert_eq!(multi_result.status.code(), Some(1));
    assert!(
        multi_stderr.contains("shared-library linking is currently available only for Mach-O")
            && !multi_stderr.contains("compilation pipeline ran"),
        "multi-input ELF link mode was not rejected before compilation:\n{multi_stderr}"
    );
    assert!(
        !multi_output.exists(),
        "rejected multi-input ELF link retained stale output"
    );

    let object = dir.join("input.o");
    let object_bytes = b"not inspected before unsupported-mode rejection";
    std::fs::write(&object, object_bytes).unwrap();
    let link_only_result = Command::new(compiler())
        .arg("-static")
        .arg(&object)
        .arg("-o")
        .arg(&object)
        .env("NO_COLOR", "1")
        .output()
        .expect("cannot run armfortas");
    assert_eq!(link_only_result.status.code(), Some(1));
    assert_eq!(
        std::fs::read(&object).unwrap(),
        object_bytes,
        "link-only unsupported-mode cleanup deleted an aliased input"
    );
    assert!(
        String::from_utf8_lossy(&link_only_result.stderr)
            .contains("static CRT/runtime discovery is not implemented"),
        "link-only rejection lost the unsupported-mode diagnostic"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
