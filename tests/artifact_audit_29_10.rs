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

fn opt_name(opt: OptLevel) -> &'static str {
    match opt {
        OptLevel::O0 => "-O0",
        OptLevel::O1 => "-O1",
        OptLevel::O2 => "-O2",
        OptLevel::O3 => "-O3",
        OptLevel::Ofast => "-Ofast",
    }
}

#[test]
fn optimized_object_snapshot_removes_inlined_helper_symbol() {
    let source = fixture("inline_pure.f90");

    let obj_o0 = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O0,
        },
        Stage::Obj,
    );
    let obj_o2 = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::Obj,
    );

    assert!(
        obj_o0.contains("external _double_it"),
        "O0 object snapshot should still export the outlined helper:\n{}",
        obj_o0
    );
    assert!(
        !obj_o2.contains("external _double_it"),
        "O2 object snapshot should reflect inlining + dead function elimination:\n{}",
        obj_o2
    );
    assert_ne!(
        obj_o0, obj_o2,
        "optimized object snapshot should differ from O0 for inline_pure"
    );
}

#[test]
fn object_snapshot_is_deterministic_with_module_globals_across_opt_levels() {
    let source = fixture("module_init.f90");

    for opt in [OptLevel::O0, OptLevel::O1, OptLevel::O2, OptLevel::O3, OptLevel::Ofast] {
        let first = capture_text(
            CaptureRequest {
                input: source.clone(),
                requested: BTreeSet::from([Stage::Obj]),
                opt_level: opt,
            },
            Stage::Obj,
        );
        let second = capture_text(
            CaptureRequest {
                input: source.clone(),
                requested: BTreeSet::from([Stage::Obj]),
                opt_level: opt,
            },
            Stage::Obj,
        );
        assert_eq!(
            first, second,
            "object snapshot should be deterministic for module globals at {}",
            opt_name(opt)
        );
    }
}

#[test]
fn linked_binary_is_deterministic_for_same_output_path_and_has_no_uuid() {
    let compiler = find_compiler();
    let source = fixture("module_init.f90");
    let stem = source.file_stem().unwrap().to_str().unwrap();

    for opt in ["-O0", "-O1", "-O2", "-O3", "-Ofast"] {
        let bin_path = std::env::temp_dir().join(format!(
            "afs_bin_det_{}_{}_{}",
            std::process::id(),
            stem,
            opt.trim_start_matches('-')
        ));

        compile_binary(&compiler, &source, opt, &bin_path);
        let load_commands = tool_output("otool", &["-l", bin_path.to_str().unwrap()]);
        assert!(
            !load_commands.contains("LC_UUID"),
            "linked binary at {} should omit LC_UUID for reproducible builds:\n{}",
            opt,
            load_commands
        );
        let first = fs::read(&bin_path).expect("cannot read first binary image");

        compile_binary(&compiler, &source, opt, &bin_path);
        let second = fs::read(&bin_path).expect("cannot read second binary image");

        assert_eq!(
            first, second,
            "linked binary should be byte-identical when rebuilt at the same output path ({})",
            opt
        );

        let _ = fs::remove_file(&bin_path);
    }
}
