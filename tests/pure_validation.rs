use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

fn compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary must be built for PURE validation tests")
}

fn unique_dir(stem: &str) -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "armfortas_pure_validation_{stem}_{}_{}",
        std::process::id(),
        id
    ));
    std::fs::create_dir_all(&dir).expect("cannot create PURE validation test directory");
    dir
}

fn write_source(dir: &Path, source: &str) -> PathBuf {
    let path = dir.join("input.f90");
    std::fs::write(&path, source).expect("cannot write PURE validation source");
    path
}

#[test]
fn impure_control_expression_calls_are_rejected_at_every_opt_level() {
    let dir = unique_dir("reject");
    let source = write_source(
        &dir,
        "\
module m
contains
  logical function impure_predicate()
    impure_predicate = .true.
  end function

  integer function impure_bound()
    impure_bound = 1
  end function

  pure subroutine exercise()
    integer :: i
    if (impure_predicate()) continue
    do while (impure_predicate())
      exit
    end do
    do i = impure_bound(), 1
    end do
    select case (impure_bound())
    case (1)
    end select
  end subroutine
end module
",
    );

    for optimization in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        let object = dir.join(format!(
            "invalid-{}.o",
            optimization.trim_start_matches('-')
        ));
        let compile = Command::new(compiler())
            .args([
                optimization,
                "-c",
                source.to_str().unwrap(),
                "-o",
                object.to_str().unwrap(),
            ])
            .output()
            .expect("failed to spawn armfortas");
        assert!(
            !compile.status.success(),
            "{optimization}: impure control-expression calls unexpectedly compiled"
        );
        let stderr = String::from_utf8_lossy(&compile.stderr);
        assert_eq!(
            stderr.matches("callee is not pure").count(),
            4,
            "{optimization}: PURE diagnostics changed:\n{stderr}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pure_control_expression_calls_compile_at_every_opt_level() {
    let dir = unique_dir("accept");
    let source = write_source(
        &dir,
        "\
module m
contains
  pure logical function pure_predicate()
    pure_predicate = .true.
  end function

  pure integer function pure_bound()
    pure_bound = 1
  end function

  pure subroutine exercise()
    integer :: i
    if (pure_predicate()) then
      do i = abs(pure_bound()), pure_bound()
      end do
    end if
  end subroutine
end module
",
    );

    for optimization in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        let object = dir.join(format!("valid-{}.o", optimization.trim_start_matches('-')));
        let compile = Command::new(compiler())
            .args([
                optimization,
                "-c",
                source.to_str().unwrap(),
                "-o",
                object.to_str().unwrap(),
            ])
            .output()
            .expect("failed to spawn armfortas");
        assert!(
            compile.status.success(),
            "{optimization}: valid PURE control-expression calls failed:\n{}",
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn unknown_and_external_pure_calls_are_rejected_at_every_opt_level() {
    let dir = unique_dir("unknown_external_reject");
    let source = write_source(
        &dir,
        "\
pure subroutine exercise()
  integer :: value
  real :: external_value
  real :: sin
  external :: external_value
  external :: sin
  external :: external_work

  call unknown_work()
  call external_work()
  value = unknown_value()
  value = int(external_value())
  value = int(sin(0.0))
end subroutine exercise
",
    );

    for optimization in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        let object = dir.join(format!(
            "invalid-contract-{}.o",
            optimization.trim_start_matches('-')
        ));
        let compile = Command::new(compiler())
            .args([
                optimization,
                "-c",
                source.to_str().unwrap(),
                "-o",
                object.to_str().unwrap(),
            ])
            .output()
            .expect("failed to spawn armfortas");
        assert!(
            !compile.status.success(),
            "{optimization}: unresolved or EXTERNAL calls unexpectedly compiled"
        );
        let stderr = String::from_utf8_lossy(&compile.stderr);
        assert_eq!(
            stderr
                .matches("requires an explicit PURE or ELEMENTAL interface")
                .count(),
            5,
            "{optimization}: PURE contract diagnostics changed:\n{stderr}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn explicit_pure_interfaces_intrinsics_and_arrays_compile_at_every_opt_level() {
    let dir = unique_dir("contract_accept");
    let source = write_source(
        &dir,
        "\
module callbacks
  abstract interface
    pure integer function pure_callback(value)
      integer, intent(in) :: value
    end function pure_callback
  end interface
contains
  pure subroutine exercise(callback, values)
    procedure(pure_callback) :: callback
    integer, intent(inout) :: values(2)
    intrinsic :: abs

    values(1) = callback(values(2))
    values(2) = abs(values(1))
  end subroutine exercise
end module callbacks
",
    );

    for optimization in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        let object = dir.join(format!(
            "valid-contract-{}.o",
            optimization.trim_start_matches('-')
        ));
        let compile = Command::new(compiler())
            .args([
                optimization,
                "-c",
                source.to_str().unwrap(),
                "-o",
                object.to_str().unwrap(),
            ])
            .output()
            .expect("failed to spawn armfortas");
        assert!(
            compile.status.success(),
            "{optimization}: valid PURE contracts failed:\n{}",
            String::from_utf8_lossy(&compile.stderr)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn pure_contract_survives_separate_module_compilation() {
    for (prefix, should_succeed) in [("pure ", true), ("", false)] {
        let dir = unique_dir(if should_succeed {
            "amod_pure"
        } else {
            "amod_impure"
        });
        let provider = dir.join("provider.f90");
        let provider_object = dir.join("provider.o");
        let consumer = dir.join("consumer.f90");
        let consumer_object = dir.join("consumer.o");
        std::fs::write(
            &provider,
            format!(
                "\
module provider
contains
  {prefix}integer function provided_value()
    provided_value = 7
  end function provided_value
end module provider
"
            ),
        )
        .expect("cannot write PURE contract provider");
        std::fs::write(
            &consumer,
            "\
module consumer
  use provider, only: provided_value
contains
  pure integer function consume_value()
    consume_value = provided_value()
  end function consume_value
end module consumer
",
        )
        .expect("cannot write PURE contract consumer");

        let provider_compile = Command::new(compiler())
            .current_dir(&dir)
            .args(["-O2", "-c", "-J", dir.to_str().unwrap()])
            .arg(&provider)
            .args(["-o", provider_object.to_str().unwrap()])
            .output()
            .expect("failed to compile PURE contract provider");
        assert!(
            provider_compile.status.success(),
            "provider compilation failed:\n{}",
            String::from_utf8_lossy(&provider_compile.stderr)
        );

        let consumer_compile = Command::new(compiler())
            .current_dir(&dir)
            .args(["-O2", "-c", "-J", dir.to_str().unwrap()])
            .arg(format!("-I{}", dir.display()))
            .arg(&consumer)
            .args(["-o", consumer_object.to_str().unwrap()])
            .output()
            .expect("failed to compile PURE contract consumer");
        assert_eq!(
            consumer_compile.status.success(),
            should_succeed,
            "separate-compilation PURE contract was not preserved:\n{}",
            String::from_utf8_lossy(&consumer_compile.stderr)
        );
        if !should_succeed {
            assert!(
                String::from_utf8_lossy(&consumer_compile.stderr).contains("callee is not pure"),
                "impure imported procedure needs a stable diagnostic:\n{}",
                String::from_utf8_lossy(&consumer_compile.stderr)
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}
