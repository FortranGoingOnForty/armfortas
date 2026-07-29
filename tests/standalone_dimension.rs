use std::collections::BTreeSet;
use std::fs;
use std::path::PathBuf;

use armfortas::driver::OptLevel;
use armfortas::testing::{capture_from_path, CaptureRequest, CapturedStage, Stage};

struct TempSource(PathBuf);

impl TempSource {
    fn new(stem: &str, source: &str) -> Self {
        let path = std::env::temp_dir().join(format!(
            "afs_standalone_dimension_{}_{}.f90",
            stem,
            std::process::id()
        ));
        fs::write(&path, source).expect("temporary Fortran source should be writable");
        Self(path)
    }

    fn path(&self) -> PathBuf {
        self.0.clone()
    }
}

impl Drop for TempSource {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.0);
    }
}

#[test]
fn standalone_dimension_executes_across_all_opt_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        armfortas::testing::report_harness_skip(
            "standalone_dimension",
            "standalone_dimension_executes_across_all_opt_levels",
            1,
            &reason,
        );
        return;
    }

    let source = TempSource::new(
        "valid",
        "module standalone_dimension_support\n\
           dimension :: x_module_values(3)\n\
         contains\n\
           subroutine verify_dimension(x_arg)\n\
             dimension x_arg(3)\n\
             if (sum(x_arg) /= 15.0) error stop 7\n\
           end subroutine verify_dimension\n\
         end module standalone_dimension_support\n\
         module standalone_dimension_real_scope\n\
         contains\n\
           subroutine shared_dimension(x_arg)\n\
             dimension x_arg(2)\n\
             x_arg = [11.0, 12.0]\n\
           end subroutine shared_dimension\n\
         end module standalone_dimension_real_scope\n\
         module standalone_dimension_integer_scope\n\
         contains\n\
           subroutine shared_dimension(i_arg)\n\
             dimension i_arg(3)\n\
             i_arg = [13, 14, 15]\n\
           end subroutine shared_dimension\n\
         end module standalone_dimension_integer_scope\n\
         program standalone_dimension\n\
           use standalone_dimension_support\n\
           use standalone_dimension_real_scope, only: real_dimension => shared_dimension\n\
           use standalone_dimension_integer_scope, only: integer_dimension => shared_dimension\n\
           interface\n\
             subroutine external_dimension(x_arg)\n\
               dimension x_arg(:)\n\
             end subroutine external_dimension\n\
           end interface\n\
           dimension :: x_values(-1:1), matrix(2, 3)\n\
           integer :: matrix\n\
           integer :: vector\n\
           real :: scoped_real(2)\n\
           integer :: scoped_integer(3)\n\
           real :: external_values(3)\n\
           dimension vector(4)\n\
           x_values = [1.0, 2.0, 3.0]\n\
           matrix = reshape([1, 2, 3, 4, 5, 6], [2, 3])\n\
           vector = [7, 8, 9, 10]\n\
           if (lbound(x_values, 1) /= -1) error stop 1\n\
           if (ubound(x_values, 1) /= 1) error stop 2\n\
           if (sum(x_values) /= 6.0) error stop 3\n\
           if (size(matrix) /= 6) error stop 4\n\
           if (matrix(2, 3) /= 6) error stop 5\n\
           if (sum(vector) /= 34) error stop 6\n\
           x_module_values = [4.0, 5.0, 6.0]\n\
           call verify_dimension(x_module_values)\n\
           call real_dimension(scoped_real)\n\
           call integer_dimension(scoped_integer)\n\
           if (sum(scoped_real) /= 23.0) error stop 9\n\
           if (sum(scoped_integer) /= 42) error stop 10\n\
           call external_dimension(external_values)\n\
           if (sum(external_values) /= 51.0) error stop 11\n\
           block\n\
             dimension y_block(2)\n\
             y_block = [2.0, 3.0]\n\
             if (sum(y_block) /= 5.0) error stop 8\n\
           end block\n\
         end program standalone_dimension\n\
         subroutine external_dimension(x_arg)\n\
           dimension x_arg(:)\n\
           x_arg = [16.0, 17.0, 18.0]\n\
         end subroutine external_dimension\n",
    );

    for opt_level in [
        OptLevel::O0,
        OptLevel::O1,
        OptLevel::O2,
        OptLevel::O3,
        OptLevel::Os,
        OptLevel::Ofast,
    ] {
        let result = capture_from_path(&CaptureRequest {
            input: source.path(),
            requested: BTreeSet::from([Stage::Run]),
            opt_level,
        })
        .unwrap_or_else(|error| {
            panic!("standalone DIMENSION should compile and run at {opt_level:?}: {error}")
        });
        let run = result
            .get(Stage::Run)
            .and_then(CapturedStage::as_run)
            .expect("missing run stage");
        assert_eq!(
            run.exit_code, 0,
            "standalone DIMENSION program failed at {opt_level:?}: {run:#?}"
        );
    }
}

#[test]
fn block_dimension_without_a_type_is_rejected_under_implicit_none() {
    let source = TempSource::new(
        "block_implicit_none",
        "program block_dimension_implicit_none\n\
           implicit none\n\
           block\n\
             implicit none\n\
             dimension x(3)\n\
           end block\n\
         end program block_dimension_implicit_none\n",
    );
    let error = capture_from_path(&CaptureRequest {
        input: source.path(),
        requested: BTreeSet::from([Stage::Ir]),
        opt_level: OptLevel::O0,
    })
    .expect_err("IMPLICIT NONE must reject an untyped BLOCK DIMENSION entity");
    assert!(
        error.detail.contains("has no implicit type"),
        "unexpected diagnostic: {error}"
    );
}
