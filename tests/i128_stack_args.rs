use std::collections::BTreeSet;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

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

fn assert_i128_return_stored_after_call(asm: &str, call_marker: &str, context: &str) {
    let call_idx = asm
        .find(call_marker)
        .unwrap_or_else(|| panic!("missing call marker '{}' in:\n{}", call_marker, asm));
    if cfg!(target_arch = "x86_64") {
        // x86 returns the i128 in rax:rdx; both limbs spill after the call.
        let tail = &asm[call_idx..];
        assert!(
            tail.contains("movq %rax, ") && tail.contains("movq %rdx, "),
            "{}:\n{}",
            context,
            asm
        );
        return;
    }
    assert!(
        asm[call_idx..].contains("stp x0, x1, [x29, #-"),
        "{}:\n{}",
        context,
        asm
    );
}

#[test]
fn internal_i128_stack_call_spills_fifth_arg_and_loads_incoming_slot_at_o0() {
    let asm = capture_text(
        CaptureRequest {
            input: fixture("integer16_internal_stack_call.f90"),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O0,
        },
        Stage::Asm,
    );

    if cfg!(target_arch = "x86_64") {
        // x86 fits three i128 args in rdi:rsi/rdx:rcx/r8:r9; the fifth
        // lands in the second outgoing stack slot (rsp+16/24) and the
        // callee reads its stack args above the saved frame.
        assert!(
            asm.contains("call afs_internal_"),
            "internal integer(16) stack-call should branch to the internalized contained helper:\n{}",
            asm
        );
        assert!(
            asm.contains("movq %rax, 16(%rsp)") && asm.contains("movq %rdx, 24(%rsp)"),
            "fifth integer(16) arg should spill to the outgoing stack area:\n{}",
            asm
        );
        assert!(
            asm.contains("movq 16(%rbp), %rax") && asm.contains("movq 32(%rbp), %rax"),
            "callee should load the incoming stack-passed integer(16) args above the frame:\n{}",
            asm
        );
        assert_i128_return_stored_after_call(
            &asm,
            "call afs_internal_",
            "caller should still receive the returned integer(16) value in rax:rdx even when args spill to the stack",
        );
        return;
    }
    assert!(
        asm.contains("bl _afs_internal_"),
        "internal integer(16) stack-call should branch to the internalized contained helper:\n{}",
        asm
    );
    assert!(
        asm.contains("stp x16, x17, [sp, #0]"),
        "fifth integer(16) arg should spill to the outgoing stack area:\n{}",
        asm
    );
    assert!(
        asm.contains("ldp x16, x17, [x29, #16]"),
        "callee should load the incoming stack-passed integer(16) arg from [x29, #16]:\n{}",
        asm
    );
    assert_i128_return_stored_after_call(
        &asm,
        "bl _afs_internal_",
        "caller should still receive the returned integer(16) value in x0/x1 even when args spill to the stack",
    );
}

#[test]
fn internal_i128_stack_call_runs_at_o0() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_stack_args test=internal_i128_stack_call_runs_at_o0 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let result = capture_from_path(&CaptureRequest {
        input: fixture("integer16_internal_stack_call.f90"),
        requested: BTreeSet::from([Stage::Run]),
        opt_level: OptLevel::O0,
    })
    .expect("internal integer(16) stack-call program should run");

    let run = result
        .get(Stage::Run)
        .and_then(CapturedStage::as_run)
        .expect("missing run capture");

    assert_eq!(
        run.exit_code, 0,
        "expected successful integer(16) stack-call run:\n{:#?}",
        run
    );
    let stdout = run.stdout_text().expect("run stdout should be UTF-8");
    assert!(
        stdout.contains('1'),
        "internal integer(16) stack-call program should print score 1:\n{}",
        stdout
    );
}

#[test]
fn internal_i128_stack_call_runs_through_optimized_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_stack_args test=internal_i128_stack_call_runs_through_optimized_levels count=1 reason=\"{}\"",
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
            input: fixture("integer16_internal_stack_call.f90"),
            requested: BTreeSet::from([Stage::Run]),
            opt_level: level,
        })
        .unwrap_or_else(|e| {
            panic!(
                "optimized integer(16) stack-call should run at {:?}:\n{}",
                level, e
            )
        });

        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run capture");

        assert_eq!(
            run.exit_code, 0,
            "expected successful integer(16) stack-call run at {:?}:\n{:#?}",
            level, run
        );
        let stdout = run.stdout_text().expect("run stdout should be UTF-8");
        assert!(
            stdout.contains('1'),
            "integer(16) stack-call program should print score 1 at {:?}:\n{}",
            level,
            stdout
        );
    }
}

#[test]
fn internal_i128_stack_call_object_snapshot_is_deterministic_at_o0() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_stack_args test=internal_i128_stack_call_object_snapshot_is_deterministic_at_o0 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("integer16_internal_stack_call.f90");
    let first = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O0,
        },
        Stage::Obj,
    );
    let second = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O0,
        },
        Stage::Obj,
    );

    assert_eq!(
        first, second,
        "internal integer(16) stack-call object snapshots should be deterministic at O0"
    );
}

#[test]
fn external_i128_stack_call_spills_fifth_arg_and_tracks_symbol_at_o0() {
    let asm = capture_text(
        CaptureRequest {
            input: fixture("integer16_external_stack_call.f90"),
            requested: BTreeSet::from([Stage::Asm]),
            opt_level: OptLevel::O0,
        },
        Stage::Asm,
    );

    if cfg!(target_arch = "x86_64") {
        assert!(
            asm.contains("call add5_ext"),
            "external integer(16) stack-call should branch to the declared symbol:\n{}",
            asm
        );
        assert!(
            asm.contains("movq %rax, 16(%rsp)") && asm.contains("movq %rdx, 24(%rsp)"),
            "fifth external integer(16) arg should spill to the outgoing stack area:\n{}",
            asm
        );
        assert_i128_return_stored_after_call(
            &asm,
            "call add5_ext",
            "external integer(16) stack-call should still receive the returned value in rax:rdx",
        );
        return;
    }
    assert!(
        asm.contains("bl _add5_ext"),
        "external integer(16) stack-call should branch to the declared symbol:\n{}",
        asm
    );
    assert!(
        asm.contains("stp x16, x17, [sp, #0]"),
        "fifth external integer(16) arg should spill to the outgoing stack area:\n{}",
        asm
    );
    assert_i128_return_stored_after_call(
        &asm,
        "bl _add5_ext",
        "external integer(16) stack-call should still receive the returned value in x0/x1",
    );
}

#[test]
fn external_i128_stack_call_object_snapshot_is_deterministic_at_o0() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=i128_stack_args test=external_i128_stack_call_object_snapshot_is_deterministic_at_o0 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let source = fixture("integer16_external_stack_call.f90");
    let first = capture_text(
        CaptureRequest {
            input: source.clone(),
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O0,
        },
        Stage::Obj,
    );
    let second = capture_text(
        CaptureRequest {
            input: source,
            requested: BTreeSet::from([Stage::Obj]),
            opt_level: OptLevel::O0,
        },
        Stage::Obj,
    );

    assert_eq!(
        first, second,
        "external integer(16) stack-call object snapshots should be deterministic at O0"
    );
}
