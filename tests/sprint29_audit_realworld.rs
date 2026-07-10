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
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary not built for this test profile")
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

fn normalize_lc_uuid(mut bytes: Vec<u8>) -> Vec<u8> {
    const MH_MAGIC_64: u32 = 0xfeedfacf;
    const LC_UUID: u32 = 0x1b;
    const MACH_HEADER_64_SIZE: usize = 32;

    if bytes.len() < MACH_HEADER_64_SIZE {
        return bytes;
    }
    let magic = u32::from_le_bytes(bytes[0..4].try_into().unwrap());
    if magic != MH_MAGIC_64 {
        return bytes;
    }
    let ncmds = u32::from_le_bytes(bytes[16..20].try_into().unwrap()) as usize;
    let mut offset = MACH_HEADER_64_SIZE;
    for _ in 0..ncmds {
        if offset + 8 > bytes.len() {
            break;
        }
        let cmd = u32::from_le_bytes(bytes[offset..offset + 4].try_into().unwrap());
        let cmdsize = u32::from_le_bytes(bytes[offset + 4..offset + 8].try_into().unwrap());
        let cmdsize = cmdsize as usize;
        if cmdsize < 8 || offset + cmdsize > bytes.len() {
            break;
        }
        if cmd == LC_UUID && cmdsize >= 24 {
            bytes[offset + 8..offset + 24].fill(0);
        }
        offset += cmdsize;
    }
    bytes
}

#[test]
fn realworld_object_snapshots_stay_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=sprint29_audit_realworld test=realworld_object_snapshots_stay_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    for name in [
        "realworld_tridiag_spmv.f90",
        "realworld_axpy_reduce.f90",
        "realworld_sasum_cleanup.f90",
        "realworld_three_point_apply.f90",
        "realworld_binomial_blend.f90",
        "realworld_shape_guard.f90",
        "realworld_affine_shift.f90",
        "realworld_noalias_reuse.f90",
        "realworld_join_bias_sum.f90",
        "realworld_seed_overwrite.f90",
        "realworld_inplace_prefix.f90",
        "realworld_inplace_symmix.f90",
        "realworld_elemental_stage.f90",
        "realworld_ipo_chain.f90",
        "realworld_doconc_square.f90",
        "realworld_vector_stage.f90",
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
        "realworld_binomial_blend.f90",
        "realworld_shape_guard.f90",
        "realworld_affine_shift.f90",
        "realworld_noalias_reuse.f90",
        "realworld_join_bias_sum.f90",
        "realworld_seed_overwrite.f90",
        "realworld_inplace_prefix.f90",
        "realworld_inplace_symmix.f90",
        "realworld_elemental_stage.f90",
        "realworld_ipo_chain.f90",
        "realworld_doconc_square.f90",
        "realworld_vector_stage.f90",
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
fn linked_realworld_binaries_are_deterministic_modulo_uuid() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=sprint29_audit_realworld test=linked_realworld_binaries_are_deterministic_modulo_uuid count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();

    for name in [
        "realworld_tridiag_spmv.f90",
        "realworld_axpy_reduce.f90",
        "realworld_sasum_cleanup.f90",
        "realworld_three_point_apply.f90",
        "realworld_binomial_blend.f90",
        "realworld_shape_guard.f90",
        "realworld_affine_shift.f90",
        "realworld_noalias_reuse.f90",
        "realworld_join_bias_sum.f90",
        "realworld_seed_overwrite.f90",
        "realworld_inplace_prefix.f90",
        "realworld_inplace_symmix.f90",
        "realworld_elemental_stage.f90",
        "realworld_ipo_chain.f90",
        "realworld_doconc_square.f90",
        "realworld_vector_stage.f90",
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
            if cfg!(target_os = "macos") {
                // LC_UUID is a Mach-O load command; ELF binaries are
                // compared raw (normalize_lc_uuid no-ops on ELF magic).
                let load_commands = tool_output("otool", &["-l", bin_path.to_str().unwrap()]);
                assert!(
                    load_commands.contains("LC_UUID"),
                    "linked binary at {} should carry LC_UUID for {}:\n{}",
                    opt,
                    name,
                    load_commands
                );
            }
            let first =
                normalize_lc_uuid(fs::read(&bin_path).expect("cannot read first binary image"));

            compile_binary(&compiler, &source, opt, &bin_path);
            let second =
                normalize_lc_uuid(fs::read(&bin_path).expect("cannot read second binary image"));

            assert_eq!(
                first, second,
                "real-world linked binary should stay deterministic modulo LC_UUID when rebuilt at the same output path ({} {})",
                name,
                opt
            );

            let _ = fs::remove_file(&bin_path);
        }
    }
}
