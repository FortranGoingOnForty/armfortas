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
fn integer16_internal_io_uses_wide_internal_symbols() {
    let source = program("integer16_internal_io.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    assert!(
        opt_ir.contains("call @afs_write_internal_int128("),
        "optimized IR should route internal integer(16) writes through the wide buffer writer:\n{}",
        opt_ir
    );
    assert!(
        opt_ir.contains("call @afs_read_internal_int128("),
        "optimized IR should route internal integer(16) reads through the wide buffer reader:\n{}",
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
    assert!(asm.contains("afs_write_internal_int128"));
    assert!(asm.contains("afs_read_internal_int128"));
}

#[test]
fn integer16_internal_io_runs_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_io_runs_across_all_opt_levels count=1 reason=\"{}\"",
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
            input: program("integer16_internal_io.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| panic!("internal integer(16) I/O should run at {:?}:\n{}", level, e));

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful internal integer(16) I/O run at {:?}:\n{:#?}",
            level, run
        );
        assert!(stdout_has_fields(
            &run.stdout,
            &["170141183460469231731687303715884105727"]
        ));
        assert!(stdout_has_fields(
            &run.stdout,
            &["-170141183460469231731687303715884105727"]
        ));
    }
}

#[test]
fn integer16_internal_io_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_io_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = program("integer16_internal_io.f90");
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
fn integer16_internal_format_uses_internal_format_sink() {
    let source = program("integer16_internal_format.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    assert!(opt_ir.contains("call @afs_fmt_begin_internal_ex("));
    assert!(opt_ir.contains("call @afs_fmt_push_int128("));

    let asm = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O2,
        },
        Stage::Asm,
    );
    assert!(asm.contains("afs_fmt_begin_internal_ex"));
    assert!(asm.contains("afs_fmt_push_int128"));
}

#[test]
fn integer16_internal_format_runs_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_format_runs_across_all_opt_levels count=1 reason=\"{}\"",
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
            input: program("integer16_internal_format.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| {
            panic!(
                "formatted internal integer(16) write should run at {:?}:\n{}",
                level, e
            )
        });

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful formatted internal integer(16) write run at {:?}:\n{:#?}",
            level, run
        );
        assert!(stdout_has_fields(
            &run.stdout,
            &["170141183460469231731687303715884105727"]
        ));
    }
}

#[test]
fn integer16_internal_format_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_format_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = program("integer16_internal_format.f90");
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
fn integer16_internal_format_read_uses_internal_format_reader() {
    let source = program("integer16_internal_format_read.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    assert!(opt_ir.contains("call @afs_fmt_read_int128_internal("));

    let asm = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O2,
        },
        Stage::Asm,
    );
    assert!(asm.contains("afs_fmt_read_int128_internal"));
}

#[test]
fn integer16_internal_format_read_runs_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_format_read_runs_across_all_opt_levels count=1 reason=\"{}\"",
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
            input: program("integer16_internal_format_read.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| {
            panic!(
                "formatted internal integer(16) read should run at {:?}:\n{}",
                level, e
            )
        });

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful formatted internal integer(16) read run at {:?}:\n{:#?}",
            level, run
        );
        assert!(stdout_has_fields(
            &run.stdout,
            &["170141183460469231731687303715884105727"]
        ));
    }
}

#[test]
fn integer16_internal_format_read_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_format_read_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = program("integer16_internal_format_read.f90");
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
fn integer16_internal_format_read_targets_use_internal_format_readers() {
    let source = program("integer16_internal_format_read_targets.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    assert!(opt_ir.contains("call @afs_fmt_read_int128_internal("));
    assert!(opt_ir.contains("call @afs_fmt_read_int_internal("));

    let asm = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O2,
        },
        Stage::Asm,
    );
    assert!(asm.contains("afs_fmt_read_int128_internal"));
    assert!(asm.contains("afs_fmt_read_int_internal"));
}

#[test]
fn integer16_internal_format_read_targets_run_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_format_read_targets_run_across_all_opt_levels count=1 reason=\"{}\"",
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
            input: program("integer16_internal_format_read_targets.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| {
            panic!(
                "formatted internal integer(16) lvalue read should run at {:?}:\n{}",
                level, e
            )
        });

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful formatted internal integer(16) lvalue read run at {:?}:\n{:#?}",
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
fn integer16_internal_format_read_targets_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_format_read_targets_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = program("integer16_internal_format_read_targets.f90");
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
fn integer16_internal_format_read_arrays_use_internal_format_readers() {
    let source = program("integer16_internal_format_read_arrays.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    assert!(opt_ir.contains("call @afs_fmt_read_int128_internal("));

    let asm = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O2,
        },
        Stage::Asm,
    );
    assert!(asm.contains("afs_fmt_read_int128_internal"));
}

#[test]
fn integer16_internal_format_read_arrays_run_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_format_read_arrays_run_across_all_opt_levels count=1 reason=\"{}\"",
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
            input: program("integer16_internal_format_read_arrays.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| {
            panic!(
                "formatted internal integer(16) array reads should run at {:?}:\n{}",
                level, e
            )
        });

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful formatted internal integer(16) array read run at {:?}:\n{:#?}",
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
fn integer16_internal_format_read_arrays_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_format_read_arrays_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = program("integer16_internal_format_read_arrays.f90");
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
fn integer16_internal_format_read_sections_use_internal_format_readers() {
    let source = program("integer16_internal_format_read_sections.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    assert!(opt_ir.contains("call @afs_fmt_read_int128_internal("));

    let asm = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O2,
        },
        Stage::Asm,
    );
    assert!(asm.contains("afs_fmt_read_int128_internal"));
}

#[test]
fn integer16_internal_format_read_sections_run_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_format_read_sections_run_across_all_opt_levels count=1 reason=\"{}\"",
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
            input: program("integer16_internal_format_read_sections.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| {
            panic!(
                "formatted internal integer(16) section reads should run at {:?}:\n{}",
                level, e
            )
        });

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful formatted internal integer(16) section read run at {:?}:\n{:#?}",
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
fn integer16_internal_format_read_sections_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_format_read_sections_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = program("integer16_internal_format_read_sections.f90");
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
fn integer16_internal_format_read_alloc_section_uses_descriptor_bounds_and_wide_reader() {
    let source = fixture("integer16_internal_format_read_alloc_section.f90");

    let ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Ir]),
            opt_level: OptLevel::O0,
        },
        Stage::Ir,
    );
    assert!(ir.contains("call @afs_fmt_read_int128_internal("));
    assert!(!ir.contains("call @afs_create_section("));

    let asm = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O2,
        },
        Stage::Asm,
    );
    assert!(asm.contains("afs_fmt_read_int128_internal"));
}

#[test]
fn integer16_internal_format_read_alloc_section_runs_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_format_read_alloc_section_runs_across_all_opt_levels count=1 reason=\"{}\"",
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
            input: fixture("integer16_internal_format_read_alloc_section.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| {
            panic!(
                "allocatable internal formatted integer(16) section read should run at {:?}:\n{}",
                level, e
            )
        });

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful allocatable internal section read run at {:?}:\n{:#?}",
            level, run
        );
        assert!(stdout_has_fields(&run.stdout, &["11"]));
    }
}

#[test]
fn integer16_internal_format_read_alloc_section_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_format_read_alloc_section_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("integer16_internal_format_read_alloc_section.f90");
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
fn integer16_internal_format_read_alloc_reverse_section_uses_wide_reader() {
    let source = fixture("integer16_internal_format_read_alloc_reverse_section.f90");

    let opt_ir = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::OptIr]),
            opt_level: OptLevel::O2,
        },
        Stage::OptIr,
    );
    assert!(opt_ir.contains("call @afs_fmt_read_int128_internal("));

    let asm = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O2,
        },
        Stage::Asm,
    );
    assert!(asm.contains("afs_fmt_read_int128_internal"));
}

#[test]
fn integer16_internal_format_read_alloc_reverse_section_runs_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_format_read_alloc_reverse_section_runs_across_all_opt_levels count=1 reason=\"{}\"",
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
            input: fixture("integer16_internal_format_read_alloc_reverse_section.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| panic!("allocatable internal formatted integer(16) reverse section read should run at {:?}:\n{}", level, e));

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful allocatable internal reverse section read run at {:?}:\n{:#?}",
            level, run
        );
        assert!(stdout_has_fields(&run.stdout, &["8"]));
    }
}

#[test]
fn integer16_internal_format_read_alloc_reverse_section_object_snapshot_is_deterministic_at_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_internal_io test=integer16_internal_format_read_alloc_reverse_section_object_snapshot_is_deterministic_at_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("integer16_internal_format_read_alloc_reverse_section.f90");
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
