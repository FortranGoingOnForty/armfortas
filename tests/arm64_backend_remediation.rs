use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

fn compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary should be built for integration tests")
}

fn compile_arm64_output(source: &str, opt: &str, emit: &str, extension: &str) -> String {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let stem = format!("armfortas_arm64_remediation_{}_{}", std::process::id(), id);
    let source_path = std::env::temp_dir().join(format!("{stem}.f90"));
    let output_path = std::env::temp_dir().join(format!("{stem}.{extension}"));
    fs::write(&source_path, source).expect("write ARM64 regression source");

    let output = Command::new(compiler())
        .args(["-ffree-form", "--target", "arm64-macos", opt, emit, "-o"])
        .arg(&output_path)
        .arg(&source_path)
        .output()
        .expect("run armfortas ARM64 compile");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "ARM64 compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let text = fs::read_to_string(&output_path).expect("read ARM64 compiler output");
    let _ = fs::remove_file(&output_path);
    text
}

fn compile_arm64_asm(source: &str, opt: &str) -> String {
    compile_arm64_output(source, opt, "-S", "s")
}

fn compile_arm64_ir(source: &str, opt: &str) -> String {
    compile_arm64_output(source, opt, "--emit-ir", "ir")
}

#[test]
fn post_call_i128_result_load_blocks_tail_call() {
    let asm = compile_arm64_asm(
        r#"
function f() result(r)
  implicit none
  integer(16) :: r
  external side
  r = 42_16
  call side()
end function
"#,
        "-O1",
    );

    let call = asm
        .find("bl _side")
        .unwrap_or_else(|| panic!("normal call must remain:\n{asm}"));
    let reload = asm[call..]
        .find("ldp x0, x1")
        .map(|offset| call + offset)
        .unwrap_or_else(|| panic!("i128 result must reload after the call:\n{asm}"));
    assert!(
        call < reload,
        "i128 result reload moved before call:\n{asm}"
    );
    assert!(
        !asm.contains("\n    b _side"),
        "call became an unsafe tail branch:\n{asm}"
    );
}

#[test]
fn overflow_arguments_block_tail_call_frame_teardown() {
    let asm = compile_arm64_asm(
        r#"
subroutine wrap() bind(c, name="wrap")
  use iso_c_binding
  interface
    subroutine sink(a1,a2,a3,a4,a5,a6,a7,a8,a9) bind(c, name="sink")
      import :: c_int
      integer(c_int), value :: a1,a2,a3,a4,a5,a6,a7,a8,a9
    end subroutine
  end interface
  call sink(11,22,33,44,55,66,77,88,99)
end subroutine
"#,
        "-O1",
    );

    assert!(
        asm.contains("bl _sink"),
        "overflow call must remain normal:\n{asm}"
    );
    assert!(
        asm.contains("[sp"),
        "ninth argument must use outgoing stack space:\n{asm}"
    );
    assert!(
        !asm.contains("\n    b _sink"),
        "frame teardown invalidates overflow arguments:\n{asm}"
    );
}

#[test]
fn incoming_i128_pair_is_captured_before_overlapping_scalar_moves() {
    let asm = compile_arm64_asm(
        r#"
function probe(a,b,c,d,w) result(r) bind(c,name="probe")
  use iso_c_binding
  integer(c_int), value :: a,b,c,d
  integer(16), value :: w
  integer(16) :: r
  r = w
end function
"#,
        "-O1",
    );

    let capture = asm
        .find("stp x4, x5")
        .unwrap_or_else(|| panic!("incoming i128 pair must be captured:\n{asm}"));
    let before_capture = &asm[..capture];
    assert!(
        !before_capture
            .lines()
            .any(|line| line.trim_start().starts_with("mov w4,")
                || line.trim_start().starts_with("mov w5,")),
        "scalar receipt clobbered x4:x5 before i128 capture:\n{asm}"
    );
}

#[test]
fn i128_arithmetic_invalidates_fused_select_flags() {
    let asm = compile_arm64_asm(
        r#"
integer(16) function f(x,y,a,b) result(r) bind(c, name="f")
  use iso_c_binding
  integer(c_int64_t), value :: x,y
  integer(16), value :: a,b
  logical :: cond
  integer(16) :: t,u
  cond = x < y
  t = a + b
  u = a - b
  r = merge(t,u,cond)
end function
"#,
        "-O1",
    );

    let lines: Vec<_> = asm.lines().collect();
    let first_csel = lines
        .iter()
        .position(|line| line.trim_start().starts_with("csel "))
        .unwrap_or_else(|| panic!("wide select must lower to CSEL:\n{asm}"));
    let last_flag_setter = lines[..first_csel]
        .iter()
        .rev()
        .find(|line| {
            let line = line.trim_start();
            line.starts_with("cmp ")
                || line.starts_with("fcmp ")
                || line.starts_with("adds ")
                || line.starts_with("subs ")
        })
        .unwrap_or_else(|| panic!("CSEL must have a preceding flag producer:\n{asm}"));
    assert!(
        last_flag_setter.trim_start().starts_with("cmp "),
        "CSEL consumed arithmetic flags instead of the condition:\n{asm}"
    );
}

#[test]
fn unsupported_arm64_where_comparisons_remain_scalar() {
    let ir = compile_arm64_ir(
        r#"
program comparisons
  use iso_fortran_env, only: int64, real32
  implicit none
  integer :: i, a(32), b(32)
  integer(int64) :: c(32), d(32)
  real(real32) :: x(32), y(32)
  do i = 1, 32
    a(i) = i
    b(i) = -i
    c(i) = int(i, int64)
    d(i) = -int(i, int64)
    x(i) = real(i, real32)
    y(i) = -real(i, real32)
  end do
  where (a /= 0)
    b = a
  end where
  where (c > 0_int64)
    d = c
  end where
  where (x /= 0.0_real32)
    y = x
  end where
  print *, b(1), d(1), y(1)
end program comparisons
"#,
        "-O3",
    );

    assert!(
        !ir.contains("vicmp") && !ir.contains("vfcmp"),
        "unsupported ARM64 comparisons reached vector instruction selection:\n{ir}"
    );
}

#[test]
fn large_frame_i128_store_preserves_low_limb() {
    let asm = compile_arm64_asm(
        r#"
function large() result(r) bind(c, name="large")
  use iso_c_binding
  integer(c_int64_t) :: pad(600)
  integer(16) :: r
  pad(1) = 7
  r = 123456789012345678901_16
end function large
"#,
        "-O0",
    );

    let value_start = asm
        .find("movz x16, #27701")
        .unwrap_or_else(|| panic!("i128 low limb was not materialized:\n{asm}"));
    let store = asm[value_start..]
        .find("str x16,")
        .map(|offset| value_start + offset)
        .unwrap_or_else(|| panic!("i128 low limb was not stored:\n{asm}"));
    assert_eq!(
        asm[value_start..store].matches("movz x16,").count(),
        1,
        "frame offset overwrote the materialized i128 low limb:\n{asm}"
    );
}
