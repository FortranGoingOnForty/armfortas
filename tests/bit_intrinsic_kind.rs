use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

fn compiler(name: &str) -> PathBuf {
    armfortas::testing::built_binary(name)
        .unwrap_or_else(|| panic!("compiler binary '{name}' not built for this test profile"))
}

fn unique_path(stem: &str, ext: &str) -> PathBuf {
    let pid = std::process::id();
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("afs_bit_intrinsic_{}_{}_{}.{}", stem, pid, id, ext))
}

fn write_program(text: &str, suffix: &str) -> PathBuf {
    let path = unique_path("src", suffix);
    std::fs::write(&path, text).expect("cannot write test source");
    path
}

#[test]
fn intrinsic_bit_result_kind_uses_actual_args_not_prior_specific() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=bit_intrinsic_kind test=intrinsic_bit_result_kind_uses_actual_args_not_prior_specific count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let src = write_program(
        "module splitmix_like\n  use iso_fortran_env, only: int8, int64\n  implicit none\n  private\n  public :: splitmix64\ncontains\n  function dist8(n) result(res)\n    integer(int8), intent(in) :: n\n    integer(int8) :: res\n    integer :: k\n    k = 64 - bit_size(n)\n    res = shiftr(source64(), k)\n  end function\n\n  function source64() result(res)\n    integer(int64) :: res\n    res = 123456789_int64\n  end function\n\n  function splitmix64() result(res)\n    integer(int64) :: res\n    integer(int64) :: int02\n    int02 = -4658895280553007687_int64\n    res = source64()\n    res = ieor(res, shiftr(res, 30)) * int02\n  end function\nend module\nprogram p\n  use iso_fortran_env, only: int64\n  use splitmix_like, only: splitmix64\n  implicit none\n  integer(int64) :: got\n  got = splitmix64()\n  if (got /= -4394453597509714643_int64) error stop 1\n  print *, 'ok'\nend program\n",
        "f90",
    );
    let out = unique_path("result_kind", "bin");
    let compile = Command::new(compiler("armfortas"))
        .args([src.to_str().unwrap(), "-o", out.to_str().unwrap()])
        .output()
        .expect("intrinsic bit result kind compile failed to spawn");
    assert!(
        compile.status.success(),
        "intrinsic bit result kind compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&out)
        .output()
        .expect("intrinsic bit result kind run failed");
    assert!(
        run.status.success() && String::from_utf8_lossy(&run.stdout).contains("ok"),
        "intrinsic bit result kind run failed: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let _ = std::fs::remove_file(&out);
    let _ = std::fs::remove_file(&src);
}
