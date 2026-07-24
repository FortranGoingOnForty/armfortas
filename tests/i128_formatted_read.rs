use std::collections::BTreeSet;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

fn program(name: &str) -> PathBuf {
    let path = PathBuf::from("test_programs").join(name);
    assert!(path.exists(), "missing program fixture {}", path.display());
    path
}

fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from("tests/fixtures").join(name);
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

fn stdout_has_fields(stdout: &[u8], expected: &[&str]) -> bool {
    let stdout = std::str::from_utf8(stdout).expect("run stdout should be UTF-8");
    let fields: Vec<_> = stdout.split_whitespace().collect();
    fields
        .windows(expected.len())
        .any(|window| window == expected)
}

#[test]
fn integer16_formatted_read_uses_wide_runtime_symbols() {
    let source = program("integer16_format_read.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    assert!(
        opt_ir.contains("call @afs_fmt_read_int128("),
        "optimized IR should route formatted integer(16) input through the wide format reader:\n{}",
        opt_ir
    );
    assert!(
        opt_ir.contains("call @afs_fmt_read_int("),
        "optimized IR should keep descriptor-indexed formatted reads for the trailing scalar:\n{}",
        opt_ir
    );

    let asm = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O2,
        },
        Stage::Asm,
    );
    assert!(asm.contains("afs_fmt_read_int128"));
    assert!(asm.contains("afs_fmt_read_int"));
}

#[test]
fn integer16_formatted_read_runs_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_formatted_read test=integer16_formatted_read_runs_across_all_opt_levels count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    for level in [
        OptLevel::O0,
        OptLevel::O1,
        OptLevel::O2,
        OptLevel::O3,
        OptLevel::Os,
        OptLevel::Ofast,
    ] {
        let result = capture_from_path(&CaptureRequest {
            input: program("integer16_format_read.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| {
            panic!(
                "formatted integer(16) input should run at {:?}:\n{}",
                level, e
            )
        });

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful formatted integer(16) read run at {:?}:\n{:#?}",
            level, run
        );
        assert!(stdout_has_fields(
            &run.stdout,
            &["170141183460469231731687303715884105727", "42"]
        ));
    }
}

#[test]
fn integer16_formatted_read_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_formatted_read test=integer16_formatted_read_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = program("integer16_format_read.f90");
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
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::Obj,
    );

    assert_eq!(first, second);
}

#[test]
fn integer16_formatted_read_targets_use_wide_runtime_symbols() {
    let source = program("integer16_format_read_targets.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    assert!(
        opt_ir.contains("call @afs_fmt_read_int128("),
        "optimized IR should route formatted integer(16) lvalue reads through the wide reader:\n{}",
        opt_ir
    );
    assert!(
        opt_ir.contains("call @afs_fmt_read_int("),
        "optimized IR should still route the trailing scalar component read through the scalar reader:\n{}",
        opt_ir
    );

    let asm = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O2,
        },
        Stage::Asm,
    );
    assert!(asm.contains("afs_fmt_read_int128"));
    assert!(asm.contains("afs_fmt_read_int"));
}

#[test]
fn integer16_formatted_read_targets_run_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_formatted_read test=integer16_formatted_read_targets_run_across_all_opt_levels count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    for level in [
        OptLevel::O0,
        OptLevel::O1,
        OptLevel::O2,
        OptLevel::O3,
        OptLevel::Os,
        OptLevel::Ofast,
    ] {
        let result = capture_from_path(&CaptureRequest {
            input: program("integer16_format_read_targets.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| {
            panic!(
                "formatted integer(16) lvalue read should run at {:?}:\n{}",
                level, e
            )
        });

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful formatted integer(16) lvalue read run at {:?}:\n{:#?}",
            level, run
        );
        for needle in [
            "11",
            "-170141183460469231731687303715884105727",
            "33",
            "9",
            "170141183460469231731687303715884105727",
            "7",
        ] {
            assert!(stdout_has_fields(&run.stdout, &[needle]));
        }
    }
}

#[test]
fn integer16_formatted_read_targets_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_formatted_read test=integer16_formatted_read_targets_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = program("integer16_format_read_targets.f90");
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
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::Obj,
    );

    assert_eq!(first, second);
}

#[test]
fn integer16_formatted_read_arrays_use_wide_runtime_symbols() {
    let source = program("integer16_format_read_arrays.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    assert!(
        opt_ir.contains("call @afs_fmt_read_int128("),
        "optimized IR should route formatted integer(16) array reads through the wide reader:\n{}",
        opt_ir
    );

    let asm = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O2,
        },
        Stage::Asm,
    );
    assert!(asm.contains("afs_fmt_read_int128"));
}

#[test]
fn integer16_formatted_read_arrays_run_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_formatted_read test=integer16_formatted_read_arrays_run_across_all_opt_levels count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    for level in [
        OptLevel::O0,
        OptLevel::O1,
        OptLevel::O2,
        OptLevel::O3,
        OptLevel::Os,
        OptLevel::Ofast,
    ] {
        let result = capture_from_path(&CaptureRequest {
            input: program("integer16_format_read_arrays.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| {
            panic!(
                "formatted integer(16) array reads should run at {:?}:\n{}",
                level, e
            )
        });

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful formatted integer(16) array read run at {:?}:\n{:#?}",
            level, run
        );
        assert!(stdout_has_fields(
            &run.stdout,
            &["11", "170141183460469231731687303715884105727", "33"]
        ));
        assert!(stdout_has_fields(
            &run.stdout,
            &["66", "-170141183460469231731687303715884105727", "44"]
        ));
    }
}

#[test]
fn integer16_formatted_read_arrays_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_formatted_read test=integer16_formatted_read_arrays_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = program("integer16_format_read_arrays.f90");
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
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::Obj,
    );

    assert_eq!(first, second);
}

#[test]
fn integer16_formatted_read_sections_use_wide_runtime_symbols() {
    let source = program("integer16_format_read_sections.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    assert!(
        opt_ir.contains("call @afs_fmt_read_int128("),
        "optimized IR should route formatted integer(16) section reads through the wide reader:\n{}",
        opt_ir
    );

    let asm = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O2,
        },
        Stage::Asm,
    );
    assert!(asm.contains("afs_fmt_read_int128"));
}

#[test]
fn integer16_formatted_read_sections_run_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_formatted_read test=integer16_formatted_read_sections_run_across_all_opt_levels count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    for level in [
        OptLevel::O0,
        OptLevel::O1,
        OptLevel::O2,
        OptLevel::O3,
        OptLevel::Os,
        OptLevel::Ofast,
    ] {
        let result = capture_from_path(&CaptureRequest {
            input: program("integer16_format_read_sections.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| {
            panic!(
                "formatted integer(16) section reads should run at {:?}:\n{}",
                level, e
            )
        });

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful formatted integer(16) section read run at {:?}:\n{:#?}",
            level, run
        );
        assert!(stdout_has_fields(&run.stdout, &["101", "202"]));
        assert!(stdout_has_fields(
            &run.stdout,
            &["606", "505", "404", "303"]
        ));
    }
}

#[test]
fn integer16_formatted_read_sections_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_formatted_read test=integer16_formatted_read_sections_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = program("integer16_format_read_sections.f90");
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
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::Obj,
    );

    assert_eq!(first, second);
}

#[test]
fn integer16_formatted_read_alloc_section_uses_descriptor_bounds_and_wide_reader() {
    let source = fixture("integer16_format_read_alloc_section.f90");

    let ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );
    assert!(ir.contains("call @afs_fmt_read_int128("));
    assert!(!ir.contains("call @afs_create_section("));

    let asm = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O2,
        },
        Stage::Asm,
    );
    assert!(asm.contains("afs_fmt_read_int128"));
}

#[test]
fn integer16_formatted_read_alloc_section_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_formatted_read test=integer16_formatted_read_alloc_section_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("integer16_format_read_alloc_section.f90");
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
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::Obj,
    );

    assert_eq!(first, second);
}

#[test]
fn integer16_formatted_read_alloc_reverse_section_uses_wide_reader() {
    let source = fixture("integer16_format_read_alloc_reverse_section.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    assert!(opt_ir.contains("call @afs_fmt_read_int128("));

    let asm = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O2,
        },
        Stage::Asm,
    );
    assert!(asm.contains("afs_fmt_read_int128"));
}

#[test]
fn integer16_formatted_read_alloc_reverse_section_runs_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_formatted_read test=integer16_formatted_read_alloc_reverse_section_runs_across_all_opt_levels count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    for level in [
        OptLevel::O1,
        OptLevel::O2,
        OptLevel::O3,
        OptLevel::Os,
        OptLevel::Ofast,
    ] {
        let result = capture_from_path(&CaptureRequest {
            input: fixture("integer16_format_read_alloc_reverse_section.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| {
            panic!(
                "allocatable formatted integer(16) reverse section read should run at {:?}:\n{}",
                level, e
            )
        });

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful allocatable reverse section read run at {:?}:\n{:#?}",
            level, run
        );
        assert!(stdout_has_fields(&run.stdout, &["8"]));
    }
}

#[test]
fn integer16_formatted_read_alloc_reverse_section_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_formatted_read test=integer16_formatted_read_alloc_reverse_section_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("integer16_format_read_alloc_reverse_section.f90");
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
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O2,
        },
        Stage::Obj,
    );

    assert_eq!(first, second);
}
