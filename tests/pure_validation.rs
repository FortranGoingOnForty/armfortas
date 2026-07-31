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
