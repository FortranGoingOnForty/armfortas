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
fn floating_point_contraction_is_ofast_only() {
    let source = r#"
real(8) function muladd(a,b,c) result(r) bind(c, name="muladd")
  use iso_c_binding
  real(c_double), value :: a,b,c
  r = a*b+c
end function
"#;

    for opt in ["-O0", "-O1", "-O2", "-O3", "-Os"] {
        let asm = compile_arm64_asm(source, opt);
        assert!(
            asm.contains("\n    fmul ") && asm.contains("\n    fadd "),
            "{opt} must preserve separate floating-point rounding:\n{asm}"
        );
        assert!(
            !asm.contains("\n    fmadd "),
            "{opt} must not contract floating-point operations:\n{asm}"
        );
    }

    let fast_asm = compile_arm64_asm(source, "-Ofast");
    assert!(
        fast_asm.contains("\n    fmadd "),
        "Ofast should contract multiply-add on ARM64:\n{fast_asm}"
    );
}

#[test]
fn vector_contraction_is_ofast_only_on_arm64() {
    let source = include_str!("../test_programs/do_loop_vectorize_fma.f90");
    let o3_asm = compile_arm64_asm(source, "-O3");
    assert!(
        o3_asm.contains("fmul.4s")
            && o3_asm.contains("fadd.4s")
            && o3_asm.contains("fmul.2d")
            && o3_asm.contains("fadd.2d"),
        "O3 should vectorize with separate multiply-add operations:\n{o3_asm}"
    );
    assert!(
        !o3_asm.contains("fmla."),
        "O3 must not contract vector operations:\n{o3_asm}"
    );

    let fast_asm = compile_arm64_asm(source, "-Ofast");
    assert_eq!(
        fast_asm.matches("fmla.").count(),
        3,
        "Ofast should contract all three vector loops:\n{fast_asm}"
    );
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
fn unsupported_arm64_i64_vector_multiply_remains_scalar() {
    let source = r#"
subroutine multiply_i64(a, b, c) bind(c, name="multiply_i64")
  use iso_c_binding, only: c_int64_t
  implicit none
  integer(c_int64_t), intent(in) :: a(32), b(32)
  integer(c_int64_t), intent(out) :: c(32)
  integer :: i
  do i = 1, 32
    c(i) = a(i) * b(i)
  end do
end subroutine multiply_i64
"#;

    let ir = compile_arm64_ir(source, "-O3");
    assert!(
        ir.contains("imul") && !ir.contains("vmul"),
        "2xi64 multiply has no NEON instruction and must remain scalar in IR:\n{ir}"
    );

    let asm = compile_arm64_asm(source, "-O3");
    assert!(
        asm.contains("\n    mul "),
        "scalar i64 multiply should reach the ARM64 MUL selector:\n{asm}"
    );
    assert!(
        !asm.lines().any(|line| line.trim() == "nop"),
        "unsupported vector arithmetic must not survive as a NOP:\n{asm}"
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

#[test]
fn optimized_logical16_storeback_preserves_value_pair() {
    let asm = compile_arm64_asm(
        include_str!("../test_programs/logical16_io_roundtrip.f90"),
        "-O1",
    );
    let lines: Vec<_> = asm.lines().map(str::trim).collect();
    let mut checked_storebacks = 0;

    for (store_index, line) in lines.iter().enumerate() {
        if !line.starts_with("stp x16, x17, [") {
            continue;
        }
        let Some(reload_index) = lines[..store_index]
            .iter()
            .rposition(|line| line.starts_with("ldr x16, [") || line.starts_with("ldp x16, x17,"))
        else {
            continue;
        };
        if !lines[reload_index..store_index]
            .iter()
            .any(|line| line.starts_with("ldr x17, [") || line.starts_with("ldp x16, x17,"))
        {
            continue;
        }

        checked_storebacks += 1;
        assert!(
            !lines[reload_index + 1..store_index]
                .iter()
                .any(|line| line.starts_with("movz x16,") || line.starts_with("movk x16,")),
            "large-offset address synthesis overwrote a live i128 value:\n{}",
            lines[reload_index..=store_index].join("\n")
        );
    }

    assert!(
        checked_storebacks > 0,
        "fixture did not exercise optimized i128 storeback:\n{asm}"
    );
}

#[test]
fn optimized_spilled_constant_keeps_all_chunks_in_one_register() {
    let asm = compile_arm64_asm(
        include_str!("../test_programs/list_read_blank_records.f90"),
        "-O1",
    );
    let lines: Vec<_> = asm.lines().map(str::trim).collect();
    let label = lines
        .iter()
        .position(|line| line.contains("_label_100_") && line.ends_with(':'))
        .unwrap_or_else(|| panic!("EOF label was not emitted:\n{asm}"));
    let block_end = lines[label + 1..]
        .iter()
        .position(|line| line.ends_with(':'))
        .map(|offset| label + 1 + offset)
        .unwrap_or(lines.len());
    let block = &lines[label + 1..block_end];
    let movz = block
        .iter()
        .position(|line| line.starts_with("movz w") && line.ends_with("#65535"))
        .unwrap_or_else(|| panic!("EOF constant low chunk was not emitted:\n{asm}"));
    let movk = block
        .iter()
        .position(|line| line.starts_with("movk w") && line.ends_with("#65535, lsl #16"))
        .unwrap_or_else(|| panic!("EOF constant high chunk was not emitted:\n{asm}"));
    let movz_reg = block[movz]
        .split_ascii_whitespace()
        .nth(1)
        .expect("MOVZ destination")
        .trim_end_matches(',');
    let movk_reg = block[movk]
        .split_ascii_whitespace()
        .nth(1)
        .expect("MOVK destination")
        .trim_end_matches(',');

    assert_eq!(
        movz_reg,
        movk_reg,
        "constant chunks changed physical registers:\n{}",
        block[..=movk].join("\n")
    );
    assert_eq!(
        movk,
        movz + 1,
        "constant materialization was split by spill code:\n{}",
        block[..=movk].join("\n")
    );
    assert!(
        block[movk + 1..]
            .iter()
            .any(|line| line.starts_with(&format!("str {movk_reg},"))),
        "fixture no longer spills the materialized constant:\n{}",
        block.join("\n")
    );
}

#[test]
fn complex_value_call_arguments_use_hfa_register_pairs() {
    let c4 = compile_arm64_asm(
        r#"
real(c_float) function call4() result(r) bind(c, name="call4")
  use iso_c_binding
  interface
    function take4(value) result(out) bind(c, name="take4")
      import :: c_float, c_float_complex
      complex(c_float_complex), value :: value
      real(c_float) :: out
    end function take4
  end interface
  r = take4(cmplx(1.25_c_float, -2.5_c_float, kind=c_float))
end function call4
"#,
        "-O0",
    );
    let call4 = c4
        .find("bl _take4")
        .unwrap_or_else(|| panic!("complex(4) call missing:\n{c4}"));
    let setup4 = &c4[..call4];
    assert!(
        setup4.contains("fmov s0,") && setup4.contains("fmov s1,"),
        "complex(4) VALUE argument must use s0:s1:\n{c4}"
    );

    let c8 = compile_arm64_asm(
        r#"
real(c_double) function call8() result(r) bind(c, name="call8")
  use iso_c_binding
  interface
    function take8(value) result(out) bind(c, name="take8")
      import :: c_double, c_double_complex
      complex(c_double_complex), value :: value
      real(c_double) :: out
    end function take8
  end interface
  r = take8(cmplx(3.5_c_double, 4.25_c_double, kind=c_double))
end function call8
"#,
        "-O0",
    );
    let call8 = c8
        .find("bl _take8")
        .unwrap_or_else(|| panic!("complex(8) call missing:\n{c8}"));
    let setup8 = &c8[..call8];
    assert!(
        setup8
            .lines()
            .any(|line| line.trim_start().starts_with("fmov d0,"))
            && setup8
                .lines()
                .any(|line| line.trim_start().starts_with("fmov d1,")),
        "complex(8) VALUE argument must use d0:d1:\n{c8}"
    );

    let mixed = compile_arm64_asm(
        r#"
real(c_double) function call_mixed() result(r) bind(c, name="call_mixed")
  use iso_c_binding
  interface
    function take_mixed(x,value,y) result(out) bind(c, name="take_mixed")
      import :: c_float, c_float_complex
      real(c_float), value :: x,y
      complex(c_float_complex), value :: value
      real(c_float) :: out
    end function take_mixed
  end interface
  r = take_mixed(1.0_c_float, &
                 cmplx(2.0_c_float, 3.0_c_float, kind=c_float), &
                 4.0_c_float)
end function call_mixed

real(c_double) function call_mixed8() result(r) bind(c, name="call_mixed8")
  use iso_c_binding
  interface
    function take_mixed8(x,value,y) result(out) bind(c, name="take_mixed8")
      import :: c_double, c_double_complex
      real(c_double), value :: x,y
      complex(c_double_complex), value :: value
      real(c_double) :: out
    end function take_mixed8
  end interface
  r = take_mixed8(1.0_c_double, &
                  cmplx(2.0_c_double, 3.0_c_double, kind=c_double), &
                  4.0_c_double)
end function call_mixed8
"#,
        "-O2",
    );
    let call = mixed
        .find("bl _take_mixed")
        .unwrap_or_else(|| panic!("mixed complex call missing:\n{mixed}"));
    let setup = &mixed[..call];
    for reg in ["s0", "s1", "s2", "s3"] {
        assert!(
            setup
                .lines()
                .any(|line| line.trim_start().starts_with(&format!("fmov {reg},"))),
            "scalar/complex/scalar arguments must occupy s0/s1:s2/s3:\n{mixed}"
        );
    }
    let call8_start = mixed
        .find("_call_mixed8:")
        .unwrap_or_else(|| panic!("mixed complex(8) caller missing:\n{mixed}"));
    let call8 = mixed[call8_start..]
        .find("bl _take_mixed8")
        .map(|offset| call8_start + offset)
        .unwrap_or_else(|| panic!("mixed complex(8) call missing:\n{mixed}"));
    let setup8 = &mixed[call8_start..call8];
    for reg in ["d0", "d1", "d2", "d3"] {
        assert!(
            setup8
                .lines()
                .any(|line| line.trim_start().starts_with(&format!("fmov {reg},"))),
            "scalar/complex/scalar arguments must occupy d0/d1:d2/d3:\n{mixed}"
        );
    }
}

#[test]
fn complex_value_parameters_are_captured_from_hfa_register_pairs() {
    let asm = compile_arm64_asm(
        r#"
real(c_float) function take4(z) result(r) bind(c, name="take4")
  use iso_c_binding
  complex(c_float_complex), value :: z
  r = real(z, c_float) + aimag(z)
end function take4

real(c_double) function take8(z) result(r) bind(c, name="take8")
  use iso_c_binding
  complex(c_double_complex), value :: z
  r = real(z, c_double) + aimag(z)
end function take8

real(c_float) function take_mixed4(x,z,y) result(r) bind(c, name="take_mixed4")
  use iso_c_binding
  real(c_float), value :: x,y
  complex(c_float_complex), value :: z
  r = x + real(z, c_float) + aimag(z) + y
end function take_mixed4

real(c_double) function take_mixed8(x,z,y) result(r) bind(c, name="take_mixed8")
  use iso_c_binding
  real(c_double), value :: x,y
  complex(c_double_complex), value :: z
  r = x + real(z, c_double) + aimag(z) + y
end function take_mixed8
"#,
        "-O2",
    );

    let take4_start = asm
        .find("_take4:")
        .unwrap_or_else(|| panic!("complex(4) callee missing:\n{asm}"));
    let take8_start = asm
        .find("_take8:")
        .unwrap_or_else(|| panic!("complex(8) callee missing:\n{asm}"));
    let mixed4_start = asm
        .find("_take_mixed4:")
        .unwrap_or_else(|| panic!("mixed complex(4) callee missing:\n{asm}"));
    let mixed8_start = asm
        .find("_take_mixed8:")
        .unwrap_or_else(|| panic!("mixed complex(8) callee missing:\n{asm}"));
    let take4 = &asm[take4_start..take8_start];
    let take8 = &asm[take8_start..mixed4_start];
    let mixed4 = &asm[mixed4_start..mixed8_start];
    let mixed8 = &asm[mixed8_start..];
    assert!(
        take4.lines().any(|line| line.trim_end().ends_with(", s0"))
            && take4.lines().any(|line| line.trim_end().ends_with(", s1")),
        "complex(4) VALUE parameter must be captured from s0:s1:\n{take4}"
    );
    assert!(
        (take8.contains("str d0,") && take8.contains("str d1,")) || take8.contains("stp d0, d1,"),
        "complex(8) VALUE parameter must be captured from d0:d1:\n{take8}"
    );
    for reg in ["s0", "s1", "s2", "s3"] {
        assert!(
            mixed4
                .lines()
                .any(|line| line.trim_end().ends_with(&format!(", {reg}"))),
            "optimized mixed complex(4) receipts must preserve {reg}:\n{mixed4}"
        );
    }
    assert!(
        mixed8.contains("stp d1, d2,")
            && mixed8.lines().any(|line| line.trim_end().ends_with(", d0"))
            && mixed8.lines().any(|line| line.trim_end().ends_with(", d3")),
        "optimized mixed complex(8) receipts must preserve d0/d1:d2/d3:\n{mixed8}"
    );
}

#[test]
fn complex_hfa_overflow_preserves_stack_layout_and_following_arguments() {
    let asm = compile_arm64_asm(
        r#"
real(c_float) function call_overflow4() result(r) bind(c, name="call_overflow4")
  use iso_c_binding
  interface
    function take_overflow4(a1,a2,a3,a4,a5,a6,a7,z,tail,marker) result(out) &
        bind(c, name="take_overflow4")
      import :: c_float, c_float_complex, c_int
      real(c_float), value :: a1,a2,a3,a4,a5,a6,a7,tail
      complex(c_float_complex), value :: z
      integer(c_int), value :: marker
      real(c_float) :: out
    end function take_overflow4
  end interface
  r = take_overflow4(1.0_c_float, 1.0_c_float, 1.0_c_float, 1.0_c_float, &
                     1.0_c_float, 1.0_c_float, 1.0_c_float, &
                     cmplx(2.0_c_float, 3.0_c_float, kind=c_float), &
                     4.0_c_float, 5_c_int)
end function call_overflow4

real(c_double) function call_overflow8() result(r) bind(c, name="call_overflow8")
  use iso_c_binding
  interface
    function take_overflow8(a1,a2,a3,a4,a5,a6,a7,z,tail,marker) result(out) &
        bind(c, name="take_overflow8")
      import :: c_double, c_double_complex, c_int
      real(c_double), value :: a1,a2,a3,a4,a5,a6,a7,tail
      complex(c_double_complex), value :: z
      integer(c_int), value :: marker
      real(c_double) :: out
    end function take_overflow8
  end interface
  r = take_overflow8(1.0_c_double, 1.0_c_double, 1.0_c_double, 1.0_c_double, &
                     1.0_c_double, 1.0_c_double, 1.0_c_double, &
                     cmplx(2.0_c_double, 3.0_c_double, kind=c_double), &
                     4.0_c_double, 5_c_int)
end function call_overflow8

real(c_float) function receive_overflow4(a1,a2,a3,a4,a5,a6,a7,z,tail,marker) &
    result(r) bind(c, name="receive_overflow4")
  use iso_c_binding
  real(c_float), value :: a1,a2,a3,a4,a5,a6,a7,tail
  complex(c_float_complex), value :: z
  integer(c_int), value :: marker
  r = real(z, c_float) + aimag(z) + tail + marker
end function receive_overflow4

real(c_double) function receive_overflow8(a1,a2,a3,a4,a5,a6,a7,z,tail,marker) &
    result(r) bind(c, name="receive_overflow8")
  use iso_c_binding
  real(c_double), value :: a1,a2,a3,a4,a5,a6,a7,tail
  complex(c_double_complex), value :: z
  integer(c_int), value :: marker
  r = real(z, c_double) + aimag(z) + tail + marker
end function receive_overflow8
"#,
        "-O2",
    );

    let call4_start = asm
        .find("_call_overflow4:")
        .unwrap_or_else(|| panic!("complex(4) overflow caller missing:\n{asm}"));
    let call8_start = asm
        .find("_call_overflow8:")
        .unwrap_or_else(|| panic!("complex(8) overflow caller missing:\n{asm}"));
    let receive4_start = asm
        .find("_receive_overflow4:")
        .unwrap_or_else(|| panic!("complex(4) overflow callee missing:\n{asm}"));
    let receive8_start = asm
        .find("_receive_overflow8:")
        .unwrap_or_else(|| panic!("complex(8) overflow callee missing:\n{asm}"));
    let call4 = &asm[call4_start..call8_start];
    let call8 = &asm[call8_start..receive4_start];
    let receive4 = &asm[receive4_start..receive8_start];
    let receive8 = &asm[receive8_start..];

    assert!(
        call4
            .lines()
            .any(|line| { line.trim_start().starts_with("str x") && line.contains("[sp, #0]") })
            && call4.lines().any(|line| {
                line.trim_start().starts_with("str s") && line.contains("[sp, #8]")
            }),
        "complex(4) overflow and following scalar need stack offsets 0 and 8:\n{call4}"
    );
    assert!(
        call8.contains("stp x16, x17, [sp, #0]")
            && call8.lines().any(|line| {
                line.trim_start().starts_with("str d") && line.contains("[sp, #16]")
            }),
        "complex(8) overflow and following scalar need stack offsets 0 and 16:\n{call8}"
    );
    assert!(
        receive4
            .lines()
            .any(|line| { line.trim_start().starts_with("ldr x") && line.contains("[x29, #16]") })
            && receive4.lines().any(|line| {
                line.trim_start().starts_with("ldr s") && line.contains("[x29, #24]")
            })
            && receive4
                .lines()
                .any(|line| line.trim_end().ends_with(", w0")),
        "complex(4) callee must receive stack overflow without consuming GP x0:\n{receive4}"
    );
    assert!(
        receive8.contains("ldp x16, x17, [x29, #16]")
            && receive8.lines().any(|line| {
                line.trim_start().starts_with("ldr d") && line.contains("[x29, #32]")
            })
            && receive8
                .lines()
                .any(|line| line.trim_end().ends_with(", w0")),
        "complex(8) callee must receive stack overflow without consuming GP x0:\n{receive8}"
    );
}
