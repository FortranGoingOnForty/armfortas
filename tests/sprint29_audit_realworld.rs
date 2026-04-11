use std::collections::BTreeSet;
use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from("test_programs").join(name);
    assert!(path.exists(), "missing test fixture {}", path.display());
    path
}

fn capture_text(request: CaptureRequest, stage: Stage) -> String {
    let result = capture_from_path(&request).expect("capture should succeed");
    match result.get(stage) {
        Some(CapturedStage::Text(text)) => text.clone(),
        Some(CapturedStage::Run(_)) => panic!("expected text stage for {}", stage.as_str()),
        None => panic!("missing requested stage {}", stage.as_str()),
    }
}

fn find_compiler() -> PathBuf {
    for candidate in ["target/debug/armfortas", "target/release/armfortas"] {
        let path = PathBuf::from(candidate);
        if path.exists() {
            return path;
        }
    }
    panic!("cannot find armfortas binary — run `cargo build` first");
}

fn compile_binary(compiler: &Path, source: &Path, opt_flag: &str, output: &Path) {
    let status = Command::new(compiler)
        .args([
            source.to_str().unwrap(),
            opt_flag,
            "-o",
            output.to_str().unwrap(),
        ])
        .status()
        .expect("compiler launch failed");
    assert!(
        status.success(),
        "binary compile failed for {} at {}",
        source.display(),
        opt_flag
    );
}

fn tool_output(tool: &str, args: &[&str]) -> String {
    let output = Command::new(tool)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run {}: {}", tool, e));
    assert!(
        output.status.success(),
        "{} failed:\n{}",
        tool,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

#[test]
fn realworld_object_snapshots_stay_deterministic_at_o2() {
    for name in [
        "realworld_tridiag_spmv.f90",
        "realworld_axpy_reduce.f90",
        "realworld_sasum_cleanup.f90",
        "realworld_three_point_apply.f90",
    ] {
        let source = fixture(name);
        let first = capture_text(
            CaptureRequest {
                input: source.clone(),
                requested: BTreeSet::from([Stage::Obj]),
                opt_level: OptLevel::O2,
            },
            Stage::Obj,
        );
        let second = capture_text(
            CaptureRequest {
                input: source.clone(),
                requested: BTreeSet::from([Stage::Obj]),
                opt_level: OptLevel::O2,
            },
            Stage::Obj,
        );
        assert_eq!(
            first, second,
            "real-world object snapshot should be deterministic at O2 for {}",
            name
        );
    }
}

#[test]
fn realworld_opt_ir_differs_from_raw_ir_at_o2() {
    for name in [
        "realworld_tridiag_spmv.f90",
        "realworld_axpy_reduce.f90",
        "realworld_sasum_cleanup.f90",
        "realworld_three_point_apply.f90",
    ] {
        let source = fixture(name);
        let raw_ir = capture_text(
            CaptureRequest {
                input: source.clone(),
                requested: BTreeSet::from([Stage::Ir]),
                opt_level: OptLevel::O0,
            },
            Stage::Ir,
        );
        let opt_ir = capture_text(
            CaptureRequest {
                input: source,
                requested: BTreeSet::from([Stage::OptIr]),
                opt_level: OptLevel::O2,
            },
            Stage::OptIr,
        );
        assert_ne!(
            raw_ir, opt_ir,
            "O2 optimized IR should materially differ from raw IR for {}",
            name
        );
    }
}

#[test]
fn linked_realworld_binaries_are_deterministic_and_uuid_free() {
    let compiler = find_compiler();

    for name in [
        "realworld_tridiag_spmv.f90",
        "realworld_axpy_reduce.f90",
        "realworld_sasum_cleanup.f90",
        "realworld_three_point_apply.f90",
    ] {
        let source = fixture(name);
        let stem = source.file_stem().unwrap().to_str().unwrap();
        for opt in ["-O0", "-O2", "-O3"] {
            let bin_path = std::env::temp_dir().join(format!(
                "afs_realworld_{}_{}_{}",
                std::process::id(),
                stem,
                opt.trim_start_matches('-')
            ));

            compile_binary(&compiler, &source, opt, &bin_path);
            let load_commands = tool_output("otool", &["-l", bin_path.to_str().unwrap()]);
            assert!(
                !load_commands.contains("LC_UUID"),
                "linked binary at {} should omit LC_UUID for {}:\n{}",
                opt,
                name,
                load_commands
            );
            let first = fs::read(&bin_path).expect("cannot read first binary image");

            compile_binary(&compiler, &source, opt, &bin_path);
            let second = fs::read(&bin_path).expect("cannot read second binary image");

            assert_eq!(
                first, second,
                "real-world linked binary should be byte-identical when rebuilt at the same output path ({} {})",
                name,
                opt
            );

            let _ = fs::remove_file(&bin_path);
        }
    }
}
