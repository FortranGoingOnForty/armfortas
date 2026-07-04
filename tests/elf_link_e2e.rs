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
    for dir in ["target/debug", "../target/debug"] {
        let p = Path::new(dir).join("armfortas");
        if p.exists() {
            return p;
        }
    }
    panic!("armfortas binary not built — run cargo build first");
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
    eprintln!(
        "\nHARNESS_SKIP suite=elf_link_e2e test={} count={} reason=\"needs an x86_64 ELF glibc or FreeBSD host with discoverable crt objects (musl: x11; NixOS: set AFS_CRT_DIR and LIBRARY_PATH)\"",
        test, count
    );
    true
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

/// Out-of-scope link paths fail with diagnostics that name the sprint
/// where they land, instead of a raw ld error.
#[test]
fn out_of_scope_link_paths_diagnose_cleanly() {
    if skip("out_of_scope_link_paths_diagnose_cleanly", 3) {
        return;
    }
    let src = programs_dir().join("hello.f90");
    let out = std::env::temp_dir().join(format!("afs_elfe2e_diag_{}", std::process::id()));

    // x16 arc: AFS_LD routing is honored on ELF now. AFS_LD=1
    // resolves the sibling afs-ld, which has no ELF support yet, so
    // the failure comes from the substitute linker rejecting the
    // input — not from a guard. AFS_LD_PATH at a real GNU ld must
    // link successfully through the routed contract.
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
        String::from_utf8_lossy(&r.stderr).contains("x11"),
        "-static should point at sprint x11"
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
