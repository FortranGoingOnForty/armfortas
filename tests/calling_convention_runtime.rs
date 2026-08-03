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
    std::env::temp_dir().join(format!("afs_callconv_{}_{}_{}.{}", stem, pid, id, ext))
}

fn unique_dir(stem: &str) -> PathBuf {
    let dir = unique_path(stem, "dir");
    std::fs::create_dir_all(&dir).expect("cannot create calling-convention test directory");
    dir
}

fn write_program_in(dir: &std::path::Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("cannot write calling-convention test source");
    path
}

fn compile_c_object(source: &std::path::Path, output: &std::path::Path) {
    let result = Command::new("clang")
        .args([
            "-fPIC",
            "-c",
            source.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .expect("failed to spawn clang");
    assert!(
        result.status.success(),
        "clang failed for {}: {}",
        source.display(),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn compile_fortran_object(source: &std::path::Path, output: &std::path::Path) {
    let result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            source.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .expect("failed to spawn armfortas object compile");
    assert!(
        result.status.success(),
        "armfortas failed for {}: {}",
        source.display(),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn compile_fortran_object_at(source: &std::path::Path, output: &std::path::Path, opt_level: &str) {
    let result = Command::new(compiler("armfortas"))
        .args(["-c", opt_level])
        .arg(source)
        .arg("-o")
        .arg(output)
        .output()
        .expect("failed to spawn armfortas object compile");
    assert!(
        result.status.success(),
        "armfortas failed for {} at {}: {}",
        source.display(),
        opt_level,
        String::from_utf8_lossy(&result.stderr)
    );
}

fn compile_fortran_object_at_f2023(
    source: &std::path::Path,
    output: &std::path::Path,
    opt_level: &str,
) {
    let result = Command::new(compiler("armfortas"))
        .args(["-c", opt_level, "--std=f2023"])
        .arg(source)
        .arg("-o")
        .arg(output)
        .output()
        .expect("failed to spawn armfortas F2023 object compile");
    assert!(
        result.status.success(),
        "armfortas failed for {} at {}: {}",
        source.display(),
        opt_level,
        String::from_utf8_lossy(&result.stderr)
    );
}

fn compile_fortran_program(source: &std::path::Path, output: &std::path::Path) {
    let result = Command::new(compiler("armfortas"))
        .args([source.to_str().unwrap(), "-o", output.to_str().unwrap()])
        .output()
        .expect("failed to spawn armfortas program compile");
    assert!(
        result.status.success(),
        "armfortas failed for {}: {}",
        source.display(),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn link_program(objects: &[&std::path::Path], output: &std::path::Path) {
    let mut cmd = Command::new(compiler("armfortas"));
    for object in objects {
        cmd.arg(object);
    }
    let result = cmd
        .args(["-o", output.to_str().unwrap()])
        .output()
        .expect("failed to spawn armfortas link");
    assert!(
        result.status.success(),
        "link failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn bind_c_dead_leading_register_arguments_survive_entry_copies() {
    const OPT_LEVELS: [&str; 6] = ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"];

    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=bind_c_dead_leading_register_arguments_survive_entry_copies count={} reason=\"{}\"",
            OPT_LEVELS.len(),
            reason
        );
        return;
    }

    let dir = unique_dir("entry_liveins");
    let c_src = write_program_in(
        &dir,
        "main.c",
        r#"#include <complex.h>
#include <stdint.h>

int32_t pick_gp4(int32_t, int32_t, int32_t, int32_t);
double pick_fp4(double, double, double, double);
__int128 echo_gp_pair(int32_t, __int128);
double consume_xmm_pair(double, double complex);
int32_t pick_gp_after_pair_revert(int32_t, int32_t, int32_t, int32_t, int32_t,
                                  __int128, int32_t);
double pick_xmm_after_pair_revert(double, double, double, double, double, double,
                                  double, double complex, double);
int32_t call_pick_gp_after_pair_revert(void);
double call_pick_xmm_after_pair_revert(void);

#if defined(__x86_64__) && defined(__ELF__)
/* C frontends disagree on reopening a register file after a two-register
   argument reverts to the stack. Drive the SysV layout directly so this
   probe isolates armfortas entry copies. */
__asm__(
    ".text\n"
    ".p2align 4\n"
    ".globl call_pick_gp_after_pair_revert\n"
    ".type call_pick_gp_after_pair_revert,@function\n"
    "call_pick_gp_after_pair_revert:\n"
    "subq $24, %rsp\n"
    "movabsq $0xfedcba9876543210, %rax\n"
    "movq %rax, 0(%rsp)\n"
    "movabsq $0x0123456789abcdef, %rax\n"
    "movq %rax, 8(%rsp)\n"
    "movl $1, %edi\n"
    "movl $2, %esi\n"
    "movl $3, %edx\n"
    "movl $4, %ecx\n"
    "movl $5, %r8d\n"
    "movl $77, %r9d\n"
    "call pick_gp_after_pair_revert\n"
    "addq $24, %rsp\n"
    "ret\n"
    ".size call_pick_gp_after_pair_revert, .-call_pick_gp_after_pair_revert\n"
    ".p2align 4\n"
    ".globl call_pick_xmm_after_pair_revert\n"
    ".type call_pick_xmm_after_pair_revert,@function\n"
    "call_pick_xmm_after_pair_revert:\n"
    "subq $24, %rsp\n"
    "movabsq $0x4020000000000000, %rax\n"
    "movq %rax, 0(%rsp)\n"
    "movabsq $0x4022000000000000, %rax\n"
    "movq %rax, 8(%rsp)\n"
    "movabsq $0x3ff0000000000000, %rax\n"
    "movq %rax, %xmm0\n"
    "movabsq $0x4000000000000000, %rax\n"
    "movq %rax, %xmm1\n"
    "movabsq $0x4008000000000000, %rax\n"
    "movq %rax, %xmm2\n"
    "movabsq $0x4010000000000000, %rax\n"
    "movq %rax, %xmm3\n"
    "movabsq $0x4014000000000000, %rax\n"
    "movq %rax, %xmm4\n"
    "movabsq $0x4018000000000000, %rax\n"
    "movq %rax, %xmm5\n"
    "movabsq $0x401c000000000000, %rax\n"
    "movq %rax, %xmm6\n"
    "movabsq $0x4025000000000000, %rax\n"
    "movq %rax, %xmm7\n"
    "call pick_xmm_after_pair_revert\n"
    "addq $24, %rsp\n"
    "ret\n"
    ".size call_pick_xmm_after_pair_revert, .-call_pick_xmm_after_pair_revert\n"
);
#else
int32_t call_pick_gp_after_pair_revert(void) {
    const __int128 wide = ((__int128)0x123456789abcdefULL << 64) |
                          (__int128)0xfedcba9876543210ULL;
    return pick_gp_after_pair_revert(1, 2, 3, 4, 5, wide, 77);
}

double call_pick_xmm_after_pair_revert(void) {
    return pick_xmm_after_pair_revert(1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0,
                                      8.0 + 9.0 * I, 10.5);
}
#endif

int main(void) {
    const __int128 wide = ((__int128)0x123456789abcdefULL << 64) |
                          (__int128)0xfedcba9876543210ULL;
    if (pick_gp4(11, 22, 33, 44) != 44) return 1;
    if (pick_fp4(1.25, 2.5, 3.75, 4.5) != 4.5) return 2;
    if (echo_gp_pair(9, wide) != wide) return 3;
    if (consume_xmm_pair(9.0, 2.0 + 3.0 * I) != 302.0) return 4;
    if (call_pick_gp_after_pair_revert() != 77) return 5;
    if (call_pick_xmm_after_pair_revert() != 10.5) return 6;
    return 0;
}
"#,
    );
    let c_obj = dir.join("main.o");
    compile_c_object(&c_src, &c_obj);

    let f_src = write_program_in(
        &dir,
        "callees.f90",
        r#"function pick_gp4(a, b, c, d) result(r) bind(c, name="pick_gp4")
  use iso_c_binding
  integer(c_int), value :: a, b, c, d
  integer(c_int) :: r
  r = d
end function pick_gp4

function pick_fp4(a, b, c, d) result(r) bind(c, name="pick_fp4")
  use iso_c_binding
  real(c_double), value :: a, b, c, d
  real(c_double) :: r
  r = d
end function pick_fp4

function echo_gp_pair(dead, wide) result(r) bind(c, name="echo_gp_pair")
  use iso_c_binding
  integer(c_int), value :: dead
  integer(16), value :: wide
  integer(16) :: r
  r = wide
end function echo_gp_pair

function consume_xmm_pair(dead, z) result(r) bind(c, name="consume_xmm_pair")
  use iso_c_binding
  real(c_double), value :: dead
  complex(c_double_complex), value :: z
  real(c_double) :: r
  r = real(z, c_double) + 100.0_c_double * aimag(z)
end function consume_xmm_pair

function pick_gp_after_pair_revert(a1, a2, a3, a4, a5, wide, tail) result(r) &
    bind(c, name="pick_gp_after_pair_revert")
  use iso_c_binding
  integer(c_int), value :: a1, a2, a3, a4, a5, tail
  integer(16), value :: wide
  integer(c_int) :: r
  r = tail
end function pick_gp_after_pair_revert

function pick_xmm_after_pair_revert(a1, a2, a3, a4, a5, a6, a7, z, tail) &
    result(r) bind(c, name="pick_xmm_after_pair_revert")
  use iso_c_binding
  real(c_double), value :: a1, a2, a3, a4, a5, a6, a7, tail
  complex(c_double_complex), value :: z
  real(c_double) :: r
  r = tail
end function pick_xmm_after_pair_revert
"#,
    );

    for opt_level in OPT_LEVELS {
        let tag = opt_level.trim_start_matches('-');
        let f_obj = dir.join(format!("callees_{tag}.o"));
        compile_fortran_object_at(&f_src, &f_obj, opt_level);

        let exe = dir.join(format!("entry_liveins_{tag}.bin"));
        link_program(&[&c_obj, &f_obj], &exe);
        let run = Command::new(&exe)
            .output()
            .unwrap_or_else(|e| panic!("cannot run {} entry-livein probe: {e}", opt_level));
        assert!(
            run.status.success(),
            "entry-livein ABI mismatch at {}: status={:?}\nstdout:\n{}\nstderr:\n{}",
            opt_level,
            run.status,
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
    }
}

#[test]
fn bind_c_narrow_returns_ignore_dirty_upper_register_bits() {
    const OPT_LEVELS: [&str; 6] = ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"];

    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=bind_c_narrow_returns_ignore_dirty_upper_register_bits count={} reason=\"{}\"",
            OPT_LEVELS.len(),
            reason
        );
        return;
    }

    let dir = unique_dir("narrow_returns");
    let c_src = write_program_in(
        &dir,
        "dirty_returns.c",
        r#"#include <stdbool.h>
#include <stdint.h>

#if defined(__x86_64__)
__attribute__((naked)) int8_t dirty_i8(void) {
    __asm__ volatile("movl $0x12345680, %eax\n\tret");
}

__attribute__((naked)) int16_t dirty_i16(void) {
    __asm__ volatile("movl $0x12348000, %eax\n\tret");
}

__attribute__((naked)) bool dirty_bool(void) {
    __asm__ volatile("movl $0x12345600, %eax\n\tret");
}
#else
int8_t dirty_i8(void) { return INT8_MIN; }
int16_t dirty_i16(void) { return INT16_MIN; }
bool dirty_bool(void) { return false; }
#endif
"#,
    );
    let c_obj = dir.join("dirty_returns.o");
    compile_c_object(&c_src, &c_obj);

    let f_src = write_program_in(
        &dir,
        "main.f90",
        r#"program p
  use iso_c_binding
  implicit none
  interface
    function dirty_i8() result(v) bind(c, name="dirty_i8")
      import :: c_signed_char
      integer(c_signed_char) :: v
    end function dirty_i8
    function dirty_i16() result(v) bind(c, name="dirty_i16")
      import :: c_short
      integer(c_short) :: v
    end function dirty_i16
    function dirty_bool() result(v) bind(c, name="dirty_bool")
      import :: c_bool
      logical(c_bool) :: v
    end function dirty_bool
  end interface

  if (dirty_i8() >= 0_c_signed_char) error stop 1
  if (dirty_i16() >= 0_c_short) error stop 2
  if (dirty_bool()) error stop 3
  print *, "ok"
end program p
"#,
    );

    for opt_level in OPT_LEVELS {
        let tag = opt_level.trim_start_matches('-');
        let f_obj = dir.join(format!("main_{tag}.o"));
        compile_fortran_object_at(&f_src, &f_obj, opt_level);

        let exe = dir.join(format!("narrow_returns_{tag}.bin"));
        link_program(&[&f_obj, &c_obj], &exe);
        let run = Command::new(&exe)
            .output()
            .unwrap_or_else(|e| panic!("cannot run {} narrow-return probe: {e}", opt_level));
        assert!(
            run.status.success(),
            "narrow return ABI mismatch at {}: status={:?}\nstdout:\n{}\nstderr:\n{}",
            opt_level,
            run.status,
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
    }
}

#[test]
fn bind_c_narrow_register_arguments_are_canonical() {
    const OPT_LEVELS: [&str; 6] = ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"];

    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=bind_c_narrow_register_arguments_are_canonical count={} reason=\"{}\"",
            OPT_LEVELS.len(),
            reason
        );
        return;
    }

    let dir = unique_dir("narrow_args");
    let c_src = write_program_in(
        &dir,
        "observe_args.c",
        r#"#include <stdbool.h>
#include <stdint.h>

#if defined(__x86_64__)
__attribute__((naked)) int32_t observe_i8(int8_t value) {
    __asm__ volatile("movl %edi, %eax\n\tret");
}

__attribute__((naked)) int32_t observe_i16(int16_t value) {
    __asm__ volatile("movl %edi, %eax\n\tret");
}

__attribute__((naked)) int32_t observe_bool(bool value) {
    __asm__ volatile("movl %edi, %eax\n\tret");
}
#else
int32_t observe_i8(int8_t value) { return value; }
int32_t observe_i16(int16_t value) { return value; }
int32_t observe_bool(bool value) { return value; }
#endif
"#,
    );
    let c_obj = dir.join("observe_args.o");
    compile_c_object(&c_src, &c_obj);

    let f_src = write_program_in(
        &dir,
        "main.f90",
        r#"program p
  use iso_c_binding
  implicit none
  interface
    function observe_i8(value) result(v) bind(c, name="observe_i8")
      import :: c_int, c_signed_char
      integer(c_signed_char), value :: value
      integer(c_int) :: v
    end function observe_i8
    function observe_i16(value) result(v) bind(c, name="observe_i16")
      import :: c_int, c_short
      integer(c_short), value :: value
      integer(c_int) :: v
    end function observe_i16
    function observe_bool(value) result(v) bind(c, name="observe_bool")
      import :: c_bool, c_int
      logical(c_bool), value :: value
      integer(c_int) :: v
    end function observe_bool
  end interface

  if (observe_i8(ibset(0_c_signed_char, 7)) /= -128_c_int) error stop 1
  if (observe_i16(ibset(0_c_short, 15)) /= -32768_c_int) error stop 2
  if (observe_bool(.false._c_bool) /= 0_c_int) error stop 3
  if (observe_bool(.true._c_bool) /= 1_c_int) error stop 4
  print *, "ok"
end program p
"#,
    );

    for opt_level in OPT_LEVELS {
        let tag = opt_level.trim_start_matches('-');
        let f_obj = dir.join(format!("main_{tag}.o"));
        compile_fortran_object_at(&f_src, &f_obj, opt_level);

        let exe = dir.join(format!("narrow_args_{tag}.bin"));
        link_program(&[&f_obj, &c_obj], &exe);
        let run = Command::new(&exe)
            .output()
            .unwrap_or_else(|e| panic!("cannot run {} narrow-argument probe: {e}", opt_level));
        assert!(
            run.status.success(),
            "narrow argument ABI mismatch at {}: status={:?}\nstdout:\n{}\nstderr:\n{}",
            opt_level,
            run.status,
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
    }
}

#[test]
fn optional_value_presence_distinguishes_omission_from_zero() {
    const OPT_LEVELS: [&str; 6] = ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"];

    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=optional_value_presence_distinguishes_omission_from_zero count={} reason=\"{}\"",
            OPT_LEVELS.len(),
            reason
        );
        return;
    }

    let dir = unique_dir("optional_value_presence");
    let src = write_program_in(
        &dir,
        "main.f90",
        r#"program p
  implicit none
  logical :: yes, no
  integer, allocatable :: values(:)

  yes = .true.
  no = .false.
  call check_integer(.false.)
  call check_integer(.true., 0)
  call check_integer(.true., (yes ? 0 : .nil.))
  call check_integer(.false., (no ? 0 : .nil.))
  call check_real(.false.)
  call check_real(.true., 0.0)
  call check_logical(.false.)
  call check_logical(.true., .false.)
  call check_with_character(.false., text='abc')
  call check_with_character(.true., 0, 'xyz')
  call check_spilled_presence(1, 2, 3, 4, 5, 6, 7, 8, .false.)
  call check_spilled_presence(1, 2, 3, 4, 5, 6, 7, 8, .true., 0)
  call forward_integer(.false.)
  call forward_integer(.true., 0)
  call forward_reference(.false.)
  call forward_reference(.true., 0)
  if (value_function(.false.) /= -1) error stop 21
  if (value_function(.true., 0) /= 0) error stop 22
  if (forward_character_reference() /= 'A') error stop 23
  if (forward_character_reference(0) /= 'P') error stop 24
  if (forward_character_value() /= 'A') error stop 25
  if (forward_character_value(0) /= 'P') error stop 26
  values = forward_array_reference()
  if (size(values) /= 1 .or. values(1) /= 0) error stop 27
  values = forward_array_reference(0)
  if (size(values) /= 1 .or. values(1) /= 1) error stop 28
  values = forward_array_value()
  if (size(values) /= 1 .or. values(1) /= 0) error stop 29
  values = forward_array_value(0)
  if (size(values) /= 1 .or. values(1) /= 1) error stop 30
  print *, 'ok'

contains

  subroutine check_integer(expect, x)
    logical, value :: expect
    integer, optional, value :: x
    if (present(x) .neqv. expect) error stop 1
    if (present(x)) then
      if (x /= 0) error stop 2
    end if
  end subroutine check_integer

  subroutine check_real(expect, x)
    logical, value :: expect
    real, optional, value :: x
    if (present(x) .neqv. expect) error stop 3
    if (present(x)) then
      if (x /= 0.0) error stop 4
    end if
  end subroutine check_real

  subroutine check_logical(expect, x)
    logical, value :: expect
    logical, optional, value :: x
    if (present(x) .neqv. expect) error stop 5
    if (present(x)) then
      if (x) error stop 6
    end if
  end subroutine check_logical

  subroutine check_with_character(expect, x, text)
    logical, value :: expect
    integer, optional, value :: x
    character(len=*), intent(in) :: text
    if (present(x) .neqv. expect) error stop 31
    if (len(text) /= 3) error stop 32
    if (text /= 'abc' .and. text /= 'xyz') error stop 33
  end subroutine check_with_character

  subroutine check_spilled_presence(a1, a2, a3, a4, a5, a6, a7, a8, expect, x)
    integer, value :: a1, a2, a3, a4, a5, a6, a7, a8
    logical, value :: expect
    integer, optional, value :: x
    if (a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 /= 36) error stop 34
    if (present(x) .neqv. expect) error stop 35
    if (present(x)) then
      if (x /= 0) error stop 36
    end if
  end subroutine check_spilled_presence

  subroutine forward_integer(expect, x)
    logical, value :: expect
    integer, optional, value :: x
    call check_integer(expect, x)
  end subroutine forward_integer

  subroutine forward_reference(expect, x)
    logical, value :: expect
    integer, optional :: x
    call check_integer(expect, x)
  end subroutine forward_reference

  integer function value_function(expect, x) result(value)
    logical, value :: expect
    integer, optional, value :: x
    if (present(x) .neqv. expect) error stop 7
    if (present(x)) then
      value = x
    else
      value = -1
    end if
  end function value_function

  character(1) function classify_character(x) result(value)
    integer, optional, value :: x
    if (present(x)) then
      value = 'P'
    else
      value = 'A'
    end if
  end function classify_character

  character(1) function forward_character_reference(x) result(value)
    integer, optional :: x
    value = classify_character(x)
  end function forward_character_reference

  character(1) function forward_character_value(x) result(value)
    integer, optional, value :: x
    value = classify_character(x)
  end function forward_character_value

  function classify_array(x) result(values)
    integer, optional, value :: x
    integer, allocatable :: values(:)
    allocate(values(1))
    if (present(x)) then
      values = 1
    else
      values = 0
    end if
  end function classify_array

  function forward_array_reference(x) result(values)
    integer, optional :: x
    integer, allocatable :: values(:)
    values = classify_array(x)
  end function forward_array_reference

  function forward_array_value(x) result(values)
    integer, optional, value :: x
    integer, allocatable :: values(:)
    values = classify_array(x)
  end function forward_array_value
end program p
"#,
    );

    for opt_level in OPT_LEVELS {
        let tag = opt_level.trim_start_matches('-');
        let obj = dir.join(format!("main_{tag}.o"));
        compile_fortran_object_at_f2023(&src, &obj, opt_level);

        let exe = dir.join(format!("optional_value_presence_{tag}.bin"));
        link_program(&[&obj], &exe);
        let run = Command::new(&exe)
            .output()
            .unwrap_or_else(|e| panic!("cannot run {} optional VALUE probe: {e}", opt_level));
        assert!(
            run.status.success(),
            "optional VALUE presence mismatch at {}: status={:?}\nstdout:\n{}\nstderr:\n{}",
            opt_level,
            run.status,
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert!(
            String::from_utf8_lossy(&run.stdout).contains("ok"),
            "unexpected optional VALUE output at {}: {}",
            opt_level,
            String::from_utf8_lossy(&run.stdout)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bind_c_optional_value_presence_matches_c_bool_abi() {
    const OPT_LEVELS: [&str; 6] = ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"];

    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=bind_c_optional_value_presence_matches_c_bool_abi count={} reason=\"{}\"",
            OPT_LEVELS.len(),
            reason
        );
        return;
    }

    let dir = unique_dir("bind_c_optional_value_presence");
    let c_src = write_program_in(
        &dir,
        "main.c",
        r#"#include <stdbool.h>
#include <stdint.h>

void optional_value_probe(int32_t value, int32_t *out, bool value_present);

int main(void) {
    int32_t out = 99;
    optional_value_probe(123, &out, false);
    if (out != -1) return 1;
    optional_value_probe(0, &out, true);
    if (out != 0) return 2;
    return 0;
}
"#,
    );
    let c_obj = dir.join("main.o");
    compile_c_object(&c_src, &c_obj);

    let f_src = write_program_in(
        &dir,
        "probe.f90",
        r#"subroutine optional_value_probe(value, out) bind(C, name="optional_value_probe")
  use iso_c_binding
  implicit none
  integer(c_int), optional, value :: value
  integer(c_int), intent(out) :: out
  if (present(value)) then
    out = value
  else
    out = -1
  end if
end subroutine optional_value_probe
"#,
    );

    for opt_level in OPT_LEVELS {
        let tag = opt_level.trim_start_matches('-');
        let f_obj = dir.join(format!("probe_{tag}.o"));
        compile_fortran_object_at(&f_src, &f_obj, opt_level);

        let exe = dir.join(format!("bind_c_optional_value_presence_{tag}.bin"));
        link_program(&[&c_obj, &f_obj], &exe);
        let run = Command::new(&exe)
            .output()
            .unwrap_or_else(|e| panic!("cannot run {} C ABI presence probe: {e}", opt_level));
        assert!(
            run.status.success(),
            "C ABI optional VALUE presence mismatch at {}: status={:?}\nstdout:\n{}\nstderr:\n{}",
            opt_level,
            run.status,
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_optional_value_presence_survives_amod_import() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=cross_tu_optional_value_presence_survives_amod_import count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let dir = unique_dir("cross_tu_optional_value_presence");
    let module_src = write_program_in(
        &dir,
        "presence_provider.f90",
        r#"module presence_provider
  implicit none
  type :: presence_checker
  contains
    procedure :: verify => verify_bound_presence
    procedure :: classify => classify_bound_presence
    procedure :: forward_reference => forward_bound_reference
    procedure :: forward_value => forward_bound_value
  end type presence_checker
contains
  subroutine verify_presence(expect, i, r, flag)
    logical, value :: expect
    integer, optional, value :: i
    real(8), optional, value :: r
    logical, optional, value :: flag
    if (present(i) .neqv. expect) error stop 1
    if (present(r) .neqv. expect) error stop 2
    if (present(flag) .neqv. expect) error stop 3
    if (expect) then
      if (i /= 0) error stop 4
      if (r /= 0.0_8) error stop 5
      if (flag) error stop 6
    end if
  end subroutine verify_presence

  subroutine verify_bound_presence(self, expect, x)
    class(presence_checker), intent(inout) :: self
    logical, value :: expect
    integer, optional, value :: x
    if (present(x) .neqv. expect) error stop 7
    if (present(x)) then
      if (x /= 0) error stop 8
    end if
  end subroutine verify_bound_presence

  integer function classify_bound_presence(self, x) result(value)
    class(presence_checker), intent(inout) :: self
    integer, optional, value :: x
    if (present(x)) then
      value = x
    else
      value = -1
    end if
  end function classify_bound_presence

  integer function forward_bound_reference(self, x) result(value)
    class(presence_checker), intent(inout) :: self
    integer, optional :: x
    value = self%classify(x)
  end function forward_bound_reference

  integer function forward_bound_value(self, x) result(value)
    class(presence_checker), intent(inout) :: self
    integer, optional, value :: x
    value = self%classify(x)
  end function forward_bound_value

  integer function classify_value(x) result(value)
    integer, optional, value :: x
    if (present(x)) then
      value = x
    else
      value = -1
    end if
  end function classify_value

  character(1) function classify_character(x) result(value)
    integer, optional, value :: x
    if (present(x)) then
      value = 'P'
    else
      value = 'A'
    end if
  end function classify_character

  character(1) function forward_character_reference(x) result(value)
    integer, optional :: x
    value = classify_character(x)
  end function forward_character_reference

  character(1) function forward_character_value(x) result(value)
    integer, optional, value :: x
    value = classify_character(x)
  end function forward_character_value
end module presence_provider
"#,
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        r#"program p
  use presence_provider, only: check => verify_presence, classify_value, &
                               classify_character, forward_character_reference, &
                               forward_character_value, presence_checker
  implicit none
  type(presence_checker) :: checker
  logical :: yes, no
  yes = .true.
  no = .false.
  call check(.false.)
  call check(expect=.true., flag=.false., r=0.0_8, i=0)
  call checker%verify(.false.)
  call checker%verify(.true., 0)
  call checker%verify(.false., (no ? 0 : .nil.))
  call checker%verify(.true., (yes ? 0 : .nil.))
  if (checker%forward_reference() /= -1) error stop 9
  if (checker%forward_reference(0) /= 0) error stop 10
  if (checker%forward_value() /= -1) error stop 19
  if (checker%forward_value(0) /= 0) error stop 20
  if (classify_value() /= -1) error stop 11
  if (classify_value(0) /= 0) error stop 12
  if (classify_character() /= 'A') error stop 13
  if (classify_character(0) /= 'P') error stop 14
  if (forward_character_reference() /= 'A') error stop 15
  if (forward_character_reference(0) /= 'P') error stop 16
  if (forward_character_value() /= 'A') error stop 17
  if (forward_character_value(0) /= 'P') error stop 18
  print *, 'ok'
end program p
"#,
    );

    let module_obj = dir.join("presence_provider.o");
    let compile_module = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args(["-c", "-O0", "-J"])
        .arg(&dir)
        .arg(&module_src)
        .arg("-o")
        .arg(&module_obj)
        .output()
        .expect("cross-TU optional VALUE module compile failed to spawn");
    assert!(
        compile_module.status.success(),
        "cross-TU optional VALUE module compile failed: {}",
        String::from_utf8_lossy(&compile_module.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args(["-c", "-O0", "--std=f2023", "-I"])
        .arg(&dir)
        .arg("-J")
        .arg(&dir)
        .arg(&main_src)
        .arg("-o")
        .arg(&main_obj)
        .output()
        .expect("cross-TU optional VALUE caller compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "cross-TU optional VALUE caller compile failed: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("cross_tu_optional_value_presence.bin");
    link_program(&[&main_obj, &module_obj], &exe);
    let run = Command::new(&exe)
        .output()
        .expect("cross-TU optional VALUE runtime failed");
    assert!(
        run.status.success(),
        "cross-TU optional VALUE runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected cross-TU optional VALUE output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn procedure_pointer_optional_value_presence_matches_interface() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=procedure_pointer_optional_value_presence_matches_interface count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let dir = unique_dir("procptr_optional_value_presence");
    let src = write_program_in(
        &dir,
        "main.f90",
        r#"module callbacks
  implicit none
  abstract interface
    subroutine callback_interface(expect, x)
      logical, value :: expect
      integer, optional, value :: x
    end subroutine callback_interface
    integer function classifier_interface(x)
      integer, optional, value :: x
    end function classifier_interface
  end interface
  type :: callback_holder
    procedure(callback_interface), pointer, nopass :: invoke => null()
    procedure(classifier_interface), pointer, nopass :: classify => null()
  end type callback_holder
contains
  subroutine callback_target(expect, x)
    logical, value :: expect
    integer, optional, value :: x
    if (present(x) .neqv. expect) error stop 1
    if (present(x)) then
      if (x /= 0) error stop 2
    end if
  end subroutine callback_target

  integer function classifier_target(x) result(value)
    integer, optional, value :: x
    if (present(x)) then
      value = x
    else
      value = -1
    end if
  end function classifier_target
end module callbacks

program p
  use callbacks
  implicit none
  type(callback_holder) :: holder
  holder%invoke => callback_target
  holder%classify => classifier_target
  call holder%invoke(.false.)
  call holder%invoke(.true., 0)
  if (holder%classify() /= -1) error stop 3
  if (holder%classify(0) /= 0) error stop 4
  print *, 'ok'
end program p
"#,
    );

    let exe = dir.join("procptr_optional_value_presence.bin");
    let compile = Command::new(compiler("armfortas"))
        .args(["-O2"])
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("procedure-pointer optional VALUE compile failed to spawn");
    assert!(
        compile.status.success(),
        "procedure-pointer optional VALUE compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&exe)
        .output()
        .expect("procedure-pointer optional VALUE runtime failed");
    assert!(
        run.status.success(),
        "procedure-pointer optional VALUE runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected procedure-pointer optional VALUE output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bind_c_mixed_gp_fp_value_args_match_c_peer() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=bind_c_mixed_gp_fp_value_args_match_c_peer count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("mixed_gp_fp");
    let c_src = write_program_in(
        &dir,
        "check_mix.c",
        "#include <stdint.h>\n\nint check_mix(int32_t a1, double d1, int32_t a2, float s1, int64_t a3, double d2, int32_t a4, float s2) {\n    if (a1 != 11) return 1;\n    if (d1 != 1.25) return 2;\n    if (a2 != 22) return 3;\n    if (s1 != 2.5f) return 4;\n    if (a3 != 33) return 5;\n    if (d2 != 3.75) return 6;\n    if (a4 != 44) return 7;\n    if (s2 != 4.25f) return 8;\n    return 0;\n}\n",
    );
    let c_obj = dir.join("check_mix.o");
    compile_c_object(&c_src, &c_obj);

    let f_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_int, c_long_long, c_float, c_double\n  implicit none\n  interface\n    function check_mix(a1, d1, a2, s1, a3, d2, a4, s2) result(rc) bind(C, name='check_mix')\n      import :: c_int, c_long_long, c_float, c_double\n      integer(c_int), value :: a1, a2, a4\n      integer(c_long_long), value :: a3\n      real(c_double), value :: d1, d2\n      real(c_float), value :: s1, s2\n      integer(c_int) :: rc\n    end function check_mix\n  end interface\n  integer(c_int) :: rc\n\n  rc = check_mix(11_c_int, 1.25_c_double, 22_c_int, 2.5_c_float, 33_c_long_long, 3.75_c_double, 44_c_int, 4.25_c_float)\n  if (rc /= 0_c_int) error stop rc\n  print *, 'ok'\nend program\n",
    );
    let f_obj = dir.join("main.o");
    compile_fortran_object(&f_src, &f_obj);

    let exe = dir.join("mixed_gp_fp.bin");
    link_program(&[&f_obj, &c_obj], &exe);

    let run = Command::new(&exe)
        .output()
        .expect("mixed GP/FP runtime failed");
    assert!(
        run.status.success(),
        "mixed GP/FP runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected mixed GP/FP output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bind_c_ninth_integer_arg_spills_with_fp_args_still_in_registers() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=bind_c_ninth_integer_arg_spills_with_fp_args_still_in_registers count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("gp_spill");
    let c_src = write_program_in(
        &dir,
        "check_gp_spill.c",
        "#include <stdint.h>\n\nint check_gp_spill(int32_t a1, double d1, int32_t a2, double d2, int32_t a3, double d3, int32_t a4, double d4, int32_t a5, int32_t a6, int32_t a7, int32_t a8, int32_t a9) {\n    if (a1 != 11) return 1;\n    if (d1 != 1.25) return 2;\n    if (a2 != 22) return 3;\n    if (d2 != 2.5) return 4;\n    if (a3 != 33) return 5;\n    if (d3 != 3.75) return 6;\n    if (a4 != 44) return 7;\n    if (d4 != 4.5) return 8;\n    if (a5 != 55) return 9;\n    if (a6 != 66) return 10;\n    if (a7 != 77) return 11;\n    if (a8 != 88) return 12;\n    if (a9 != 99) return 13;\n    return 0;\n}\n",
    );
    let c_obj = dir.join("check_gp_spill.o");
    compile_c_object(&c_src, &c_obj);

    let f_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_int, c_double\n  implicit none\n  interface\n    function check_gp_spill(a1, d1, a2, d2, a3, d3, a4, d4, a5, a6, a7, a8, a9) result(rc) bind(C, name='check_gp_spill')\n      import :: c_int, c_double\n      integer(c_int), value :: a1, a2, a3, a4, a5, a6, a7, a8, a9\n      real(c_double), value :: d1, d2, d3, d4\n      integer(c_int) :: rc\n    end function check_gp_spill\n  end interface\n  integer(c_int) :: rc\n\n  rc = check_gp_spill(11_c_int, 1.25_c_double, 22_c_int, 2.5_c_double, 33_c_int, 3.75_c_double, 44_c_int, 4.5_c_double, 55_c_int, 66_c_int, 77_c_int, 88_c_int, 99_c_int)\n  if (rc /= 0_c_int) error stop rc\n  print *, 'ok'\nend program\n",
    );
    let f_obj = dir.join("main.o");
    compile_fortran_object(&f_src, &f_obj);

    let exe = dir.join("gp_spill.bin");
    link_program(&[&f_obj, &c_obj], &exe);

    let run = Command::new(&exe)
        .output()
        .expect("GP spill runtime failed");
    assert!(
        run.status.success(),
        "GP spill runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected GP spill output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bind_c_ninth_float_arg_spills_with_integer_args_still_in_registers() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=bind_c_ninth_float_arg_spills_with_integer_args_still_in_registers count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("fp_spill");
    let c_src = write_program_in(
        &dir,
        "check_fp_spill.c",
        "#include <stdint.h>\n\nint check_fp_spill(double d1, int32_t a1, double d2, int32_t a2, double d3, int32_t a3, double d4, int32_t a4, double d5, double d6, double d7, double d8, double d9) {\n    if (d1 != 1.25) return 1;\n    if (a1 != 11) return 2;\n    if (d2 != 2.5) return 3;\n    if (a2 != 22) return 4;\n    if (d3 != 3.75) return 5;\n    if (a3 != 33) return 6;\n    if (d4 != 4.5) return 7;\n    if (a4 != 44) return 8;\n    if (d5 != 5.25) return 9;\n    if (d6 != 6.5) return 10;\n    if (d7 != 7.75) return 11;\n    if (d8 != 8.5) return 12;\n    if (d9 != 9.25) return 13;\n    return 0;\n}\n",
    );
    let c_obj = dir.join("check_fp_spill.o");
    compile_c_object(&c_src, &c_obj);

    let f_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_int, c_double\n  implicit none\n  interface\n    function check_fp_spill(d1, a1, d2, a2, d3, a3, d4, a4, d5, d6, d7, d8, d9) result(rc) bind(C, name='check_fp_spill')\n      import :: c_int, c_double\n      real(c_double), value :: d1, d2, d3, d4, d5, d6, d7, d8, d9\n      integer(c_int), value :: a1, a2, a3, a4\n      integer(c_int) :: rc\n    end function check_fp_spill\n  end interface\n  integer(c_int) :: rc\n\n  rc = check_fp_spill(1.25_c_double, 11_c_int, 2.5_c_double, 22_c_int, 3.75_c_double, 33_c_int, 4.5_c_double, 44_c_int, 5.25_c_double, 6.5_c_double, 7.75_c_double, 8.5_c_double, 9.25_c_double)\n  if (rc /= 0_c_int) error stop rc\n  print *, 'ok'\nend program\n",
    );
    let f_obj = dir.join("main.o");
    compile_fortran_object(&f_src, &f_obj);

    let exe = dir.join("fp_spill.bin");
    link_program(&[&f_obj, &c_obj], &exe);

    let run = Command::new(&exe)
        .output()
        .expect("FP spill runtime failed");
    assert!(
        run.status.success(),
        "FP spill runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected FP spill output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bind_c_signed_char_value_args_keep_narrow_stack_widths() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=bind_c_signed_char_value_args_keep_narrow_stack_widths count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("i8_stack");
    let c_src = write_program_in(
        &dir,
        "check_i8_stack.c",
        "#include <stdint.h>\n\nint check_i8_stack(int8_t a1, int8_t a2, int8_t a3, int8_t a4, int8_t a5, int8_t a6, int8_t a7, int8_t a8, int8_t a9, int8_t a10) {\n    if (a1 != 1) return 1;\n    if (a2 != 2) return 2;\n    if (a3 != 3) return 3;\n    if (a4 != 4) return 4;\n    if (a5 != 5) return 5;\n    if (a6 != 6) return 6;\n    if (a7 != 7) return 7;\n    if (a8 != 8) return 8;\n    if (a9 != 9) return 9;\n    if (a10 != 10) return 10;\n    return 19;\n}\n",
    );
    let c_obj = dir.join("check_i8_stack.o");
    compile_c_object(&c_src, &c_obj);

    let f_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_int, c_signed_char\n  implicit none\n  interface\n    function check_i8_stack(a1, a2, a3, a4, a5, a6, a7, a8, a9, a10) result(rc) bind(C, name='check_i8_stack')\n      import :: c_int, c_signed_char\n      integer(c_signed_char), value :: a1, a2, a3, a4, a5, a6, a7, a8, a9, a10\n      integer(c_int) :: rc\n    end function check_i8_stack\n  end interface\n  integer(c_int) :: rc\n\n  rc = check_i8_stack(1_c_signed_char, 2_c_signed_char, 3_c_signed_char, 4_c_signed_char, 5_c_signed_char, 6_c_signed_char, 7_c_signed_char, 8_c_signed_char, 9_c_signed_char, 10_c_signed_char)\n  if (rc /= 19_c_int) error stop rc\n  print *, 'ok'\nend program\n",
    );
    let f_obj = dir.join("main.o");
    compile_fortran_object(&f_src, &f_obj);

    let exe = dir.join("i8_stack.bin");
    link_program(&[&f_obj, &c_obj], &exe);

    let run = Command::new(&exe)
        .output()
        .expect("c_signed_char stack runtime failed");
    assert!(
        run.status.success(),
        "c_signed_char stack runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected c_signed_char stack output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bind_c_short_value_args_keep_narrow_stack_widths() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=bind_c_short_value_args_keep_narrow_stack_widths count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("i16_stack");
    let c_src = write_program_in(
        &dir,
        "check_i16_stack.c",
        "#include <stdint.h>\n\nint check_i16_stack(int16_t a1, int16_t a2, int16_t a3, int16_t a4, int16_t a5, int16_t a6, int16_t a7, int16_t a8, int16_t a9, int16_t a10) {\n    if (a1 != 1) return 1;\n    if (a2 != 2) return 2;\n    if (a3 != 3) return 3;\n    if (a4 != 4) return 4;\n    if (a5 != 5) return 5;\n    if (a6 != 6) return 6;\n    if (a7 != 7) return 7;\n    if (a8 != 8) return 8;\n    if (a9 != 9) return 9;\n    if (a10 != 10) return 10;\n    return 19;\n}\n",
    );
    let c_obj = dir.join("check_i16_stack.o");
    compile_c_object(&c_src, &c_obj);

    let f_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_int, c_short\n  implicit none\n  interface\n    function check_i16_stack(a1, a2, a3, a4, a5, a6, a7, a8, a9, a10) result(rc) bind(C, name='check_i16_stack')\n      import :: c_int, c_short\n      integer(c_short), value :: a1, a2, a3, a4, a5, a6, a7, a8, a9, a10\n      integer(c_int) :: rc\n    end function check_i16_stack\n  end interface\n  integer(c_int) :: rc\n\n  rc = check_i16_stack(1_c_short, 2_c_short, 3_c_short, 4_c_short, 5_c_short, 6_c_short, 7_c_short, 8_c_short, 9_c_short, 10_c_short)\n  if (rc /= 19_c_int) error stop rc\n  print *, 'ok'\nend program\n",
    );
    let f_obj = dir.join("main.o");
    compile_fortran_object(&f_src, &f_obj);

    let exe = dir.join("i16_stack.bin");
    link_program(&[&f_obj, &c_obj], &exe);

    let run = Command::new(&exe)
        .output()
        .expect("c_short stack runtime failed");
    assert!(
        run.status.success(),
        "c_short stack runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected c_short stack output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn contained_hidden_result_optional_gap_preserves_host_and_char_ordering() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=contained_hidden_result_optional_gap_preserves_host_and_char_ordering count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("contained_hidden_result_gap");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  character(len=32) :: out\n  out = outer('cmd', 'abc')\n  if (trim(out) /= 'cmd=5') error stop 1\n  print *, trim(out)\ncontains\n  function outer(name, value) result(line)\n    character(len=*), intent(in) :: name, value\n    character(len=32) :: line\n    integer :: bias\n    bias = 2\n    line = render(name, value)\n  contains\n    function render(name, value, manual_len) result(out)\n      character(len=*), intent(in) :: name, value\n      integer, intent(in), optional :: manual_len\n      character(len=32) :: out\n      integer :: n\n      if (present(manual_len)) then\n        n = manual_len + bias\n      else\n        n = len_trim(value) + bias\n      end if\n      write(out, '(A,I0)') trim(name) // '=', n\n    end function render\n  end function outer\nend program\n",
    );
    let exe = dir.join("contained_hidden_result_gap.bin");
    compile_fortran_program(&src, &exe);

    let run = Command::new(&exe)
        .output()
        .expect("contained hidden-result runtime failed");
    assert!(
        run.status.success(),
        "contained hidden-result runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("cmd=5"),
        "unexpected contained hidden-result output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recursive_contained_helper_preserves_host_closure_and_hidden_lengths() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=recursive_contained_helper_preserves_host_closure_and_hidden_lengths count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("recursive_hidden_lengths");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer :: total\n  total = walk(3, 2.0d0, 'abc')\n  if (total /= 9) error stop 1\n  print *, total\ncontains\n  recursive integer function walk(n, scale, label) result(total)\n    integer, intent(in) :: n\n    real(8), intent(in) :: scale\n    character(len=*), intent(in) :: label\n    integer :: bias\n    bias = int(scale)\n    if (n <= 0) then\n      total = len_trim(label)\n    else\n      total = helper(n, label)\n    end if\n  contains\n    integer function helper(n, label) result(step)\n      integer, intent(in) :: n\n      character(len=*), intent(in) :: label\n      step = bias + walk(n - 1, scale, label)\n    end function helper\n  end function walk\nend program\n",
    );
    let exe = dir.join("recursive_hidden_lengths.bin");
    compile_fortran_program(&src, &exe);

    let run = Command::new(&exe)
        .output()
        .expect("recursive contained helper runtime failed");
    assert!(
        run.status.success(),
        "recursive contained helper runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("9"),
        "unexpected recursive contained helper output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bind_c_keyword_reordering_preserves_mixed_value_and_byref_slots() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=bind_c_keyword_reordering_preserves_mixed_value_and_byref_slots count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("keyword_mix");
    let c_src = write_program_in(
        &dir,
        "check_keyword_mix.c",
        "#include <stdint.h>\n\nint check_keyword_mix(int32_t *out_sum, int32_t a, double d, int32_t *inout, float s, int64_t big) {\n    if (!out_sum || !inout) return 100;\n    if (a != 7) return 1;\n    if (d != 1.5) return 2;\n    if (*inout != 10) return 3;\n    if (s != 2.25f) return 4;\n    if (big != 99) return 5;\n    *out_sum = a + *inout + (int32_t)big + (int32_t)d + (int32_t)s;\n    *inout += 5;\n    return 0;\n}\n",
    );
    let c_obj = dir.join("check_keyword_mix.o");
    compile_c_object(&c_src, &c_obj);

    let f_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_int, c_long_long, c_float, c_double\n  implicit none\n  interface\n    function check_keyword_mix(out_sum, a, d, inout, s, big) result(rc) bind(C, name='check_keyword_mix')\n      import :: c_int, c_long_long, c_float, c_double\n      integer(c_int), intent(out) :: out_sum\n      integer(c_int), value :: a\n      real(c_double), value :: d\n      integer(c_int), intent(inout) :: inout\n      real(c_float), value :: s\n      integer(c_long_long), value :: big\n      integer(c_int) :: rc\n    end function check_keyword_mix\n  end interface\n  integer(c_int) :: rc, out_sum, inout\n\n  inout = 10_c_int\n  rc = check_keyword_mix(big=99_c_long_long, s=2.25_c_float, inout=inout, d=1.5_c_double, out_sum=out_sum, a=7_c_int)\n  if (rc /= 0_c_int) error stop rc\n  if (out_sum /= 119_c_int) error stop 11\n  if (inout /= 15_c_int) error stop 12\n  print *, 'ok'\nend program\n",
    );
    let f_obj = dir.join("main.o");
    compile_fortran_object(&f_src, &f_obj);

    let exe = dir.join("keyword_mix.bin");
    link_program(&[&f_obj, &c_obj], &exe);

    let run = Command::new(&exe)
        .output()
        .expect("keyword mixed-slot runtime failed");
    assert!(
        run.status.success(),
        "keyword mixed-slot runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected keyword mixed-slot output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn bind_c_keyword_reordering_preserves_gp_spill_with_pointer_args() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=bind_c_keyword_reordering_preserves_gp_spill_with_pointer_args count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("keyword_gp_spill");
    let c_src = write_program_in(
        &dir,
        "check_keyword_spill.c",
        "#include <stdint.h>\n\nint check_keyword_spill(int32_t *out0, int32_t a1, int32_t *io1, double d1, int32_t a2, int32_t *io2, double d2, int32_t a3, int32_t *io3, double d3, int32_t a4, int32_t *io4, int32_t a5) {\n    if (!out0 || !io1 || !io2 || !io3 || !io4) return 100;\n    if (a1 != 11) return 1;\n    if (*io1 != 101) return 2;\n    if (d1 != 1.25) return 3;\n    if (a2 != 22) return 4;\n    if (*io2 != 202) return 5;\n    if (d2 != 2.5) return 6;\n    if (a3 != 33) return 7;\n    if (*io3 != 303) return 8;\n    if (d3 != 3.75) return 9;\n    if (a4 != 44) return 10;\n    if (*io4 != 404) return 11;\n    if (a5 != 55) return 12;\n    *out0 = a1 + a2 + a3 + a4 + a5;\n    *io4 += 1;\n    return 0;\n}\n",
    );
    let c_obj = dir.join("check_keyword_spill.o");
    compile_c_object(&c_src, &c_obj);

    let f_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_int, c_double\n  implicit none\n  interface\n    function check_keyword_spill(out0, a1, io1, d1, a2, io2, d2, a3, io3, d3, a4, io4, a5) result(rc) bind(C, name='check_keyword_spill')\n      import :: c_int, c_double\n      integer(c_int), intent(out) :: out0\n      integer(c_int), value :: a1, a2, a3, a4, a5\n      integer(c_int), intent(inout) :: io1, io2, io3, io4\n      real(c_double), value :: d1, d2, d3\n      integer(c_int) :: rc\n    end function check_keyword_spill\n  end interface\n  integer(c_int) :: rc, out0, io1, io2, io3, io4\n\n  io1 = 101_c_int\n  io2 = 202_c_int\n  io3 = 303_c_int\n  io4 = 404_c_int\n  rc = check_keyword_spill(a5=55_c_int, io2=io2, d2=2.5_c_double, out0=out0, a1=11_c_int, io1=io1, d1=1.25_c_double, a2=22_c_int, io3=io3, d3=3.75_c_double, a3=33_c_int, io4=io4, a4=44_c_int)\n  if (rc /= 0_c_int) error stop rc\n  if (out0 /= 165_c_int) error stop 21\n  if (io4 /= 405_c_int) error stop 22\n  print *, 'ok'\nend program\n",
    );
    let f_obj = dir.join("main.o");
    compile_fortran_object(&f_src, &f_obj);

    let exe = dir.join("keyword_gp_spill.bin");
    link_program(&[&f_obj, &c_obj], &exe);

    let run = Command::new(&exe)
        .output()
        .expect("keyword GP spill runtime failed");
    assert!(
        run.status.success(),
        "keyword GP spill runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ok"),
        "unexpected keyword GP spill output: {}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn recursive_non_bindc_calls_preserve_hidden_lengths_host_closure_and_gp_spills() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=recursive_non_bindc_calls_preserve_hidden_lengths_host_closure_and_gp_spills count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("recursive_non_bindc_gp_spill");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer :: total\n  total = outer()\n  if (total /= 1512) error stop 1\n  print *, total\ncontains\n  integer function outer() result(total)\n    integer :: bias\n    bias = 3\n    total = walk(2, 'abc', 11, 22, 33, 44, 55, 66, 77, 88, 99, 1.25d0, 2.5d0)\n  contains\n    recursive integer function walk(n, tag, a1, a2, a3, a4, a5, a6, a7, a8, a9, d1, d2) result(v)\n      integer, intent(in) :: n\n      character(len=*), intent(in) :: tag\n      integer, intent(in) :: a1, a2, a3, a4, a5, a6, a7, a8, a9\n      real(8), intent(in) :: d1, d2\n      if (n <= 0) then\n        v = leaf(a8=a8, d2=d2, a3=a3, a5=a5, a1=a1, tag=tag, a9=a9, d1=d1, a7=a7, a2=a2, a4=a4, a6=a6)\n      else\n        v = leaf(a8=a8, d2=d2, a3=a3, a5=a5, a1=a1, tag=tag, a9=a9, d1=d1, a7=a7, a2=a2, a4=a4, a6=a6) + &\n            walk(n - 1, tag, a1, a2, a3, a4, a5, a6, a7, a8, a9, d1, d2)\n      end if\n    contains\n      integer function leaf(tag, a1, a2, a3, a4, a5, a6, a7, a8, a9, d1, d2) result(sumv)\n        character(len=*), intent(in) :: tag\n        integer, intent(in) :: a1, a2, a3, a4, a5, a6, a7, a8, a9\n        real(8), intent(in) :: d1, d2\n        sumv = a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 + a9 + int(d1) + int(d2) + len_trim(tag) + bias\n      end function leaf\n    end function walk\n  end function outer\nend program\n",
    );
    let exe = dir.join("recursive_non_bindc_gp_spill.bin");
    compile_fortran_program(&src, &exe);

    let run = Command::new(&exe)
        .output()
        .expect("recursive non-bind(c) GP spill runtime failed");
    assert!(
        run.status.success(),
        "recursive non-bind(c) GP spill runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("1512"),
        "unexpected recursive non-bind(c) GP spill output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn non_bindc_keyword_reordering_preserves_mixed_gp_fp_spills() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=non_bindc_keyword_reordering_preserves_mixed_gp_fp_spills count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("non_bindc_keyword_spills");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer :: total\n  total = driver()\n  if (total /= 542) error stop 1\n  print *, total\ncontains\n  integer function driver() result(total)\n    total = accumulate(a9=99, d8=8.5d0, a4=44, d2=2.5d0, tag='xy', a1=11, d5=5.25d0, a7=77, d1=1.25d0, &\n                       a2=22, d9=9.25d0, a5=55, d4=4.5d0, a8=88, a3=33, d6=6.5d0, a6=66, d3=3.75d0, d7=7.75d0)\n  contains\n    integer function accumulate(tag, a1, a2, a3, a4, a5, a6, a7, a8, a9, d1, d2, d3, d4, d5, d6, d7, d8, d9) result(v)\n      character(len=*), intent(in) :: tag\n      integer, intent(in) :: a1, a2, a3, a4, a5, a6, a7, a8, a9\n      real(8), intent(in) :: d1, d2, d3, d4, d5, d6, d7, d8, d9\n      v = a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 + a9 + int(d1) + int(d2) + int(d3) + int(d4) + int(d5) + int(d6) + int(d7) + int(d8) + int(d9) + len_trim(tag)\n    end function accumulate\n  end function driver\nend program\n",
    );
    let exe = dir.join("non_bindc_keyword_spills.bin");
    compile_fortran_program(&src, &exe);

    let run = Command::new(&exe)
        .output()
        .expect("non-bind(c) keyword spill runtime failed");
    assert!(
        run.status.success(),
        "non-bind(c) keyword spill runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("542"),
        "unexpected non-bind(c) keyword spill output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_character_result_with_spills_survives_amod_import() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=cross_tu_character_result_with_spills_survives_amod_import count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("cross_tu_char_result_spills");
    let mod_src = write_program_in(
        &dir,
        "abi_mod.f90",
        "module abi_mod\ncontains\n  function accumulate(tag, a1, a2, a3, a4, a5, a6, a7, a8, a9, d1, d2, d3, d4, d5, d6, d7, d8, d9) result(out)\n    character(len=*), intent(in) :: tag\n    integer, intent(in) :: a1, a2, a3, a4, a5, a6, a7, a8, a9\n    real(8), intent(in) :: d1, d2, d3, d4, d5, d6, d7, d8, d9\n    character(len=32) :: out\n    write(out, '(A,I0)') trim(tag) // '=', a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 + a9 + int(d1) + int(d2) + int(d3) + int(d4) + int(d5) + int(d6) + int(d7) + int(d8) + int(d9)\n  end function accumulate\nend module abi_mod\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use abi_mod, only: accumulate\n  implicit none\n  character(len=32) :: out\n  out = accumulate(a9=99, d8=8.5d0, a4=44, d2=2.5d0, tag='xy', a1=11, d5=5.25d0, a7=77, d1=1.25d0, &\n                   a2=22, d9=9.25d0, a5=55, d4=4.5d0, a8=88, a3=33, d6=6.5d0, a6=66, d3=3.75d0, d7=7.75d0)\n  if (trim(out) /= 'xy=540') error stop 1\n  print *, trim(out)\nend program\n",
    );

    let mod_obj = dir.join("abi_mod.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("cross-TU char-result module compile failed to spawn");
    assert!(
        compile_mod.status.success(),
        "cross-TU char-result module compile failed: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("cross-TU char-result main compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "cross-TU char-result main compile failed: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("cross_tu_char_result_spills.bin");
    link_program(&[&main_obj, &mod_obj], &exe);

    let run = Command::new(&exe)
        .output()
        .expect("cross-TU char-result runtime failed");
    assert!(
        run.status.success(),
        "cross-TU char-result runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("xy=540"),
        "unexpected cross-TU char-result output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_bindc_keyword_spills_survive_amod_import() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=cross_tu_bindc_keyword_spills_survive_amod_import count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("cross_tu_bindc_keyword_spills");
    let c_src = write_program_in(
        &dir,
        "check_keyword_spill.c",
        "#include <stdint.h>\n\nint check_keyword_spill(int32_t *out0, int32_t a1, int32_t *io1, double d1, int32_t a2, int32_t *io2, double d2, int32_t a3, int32_t *io3, double d3, int32_t a4, int32_t *io4, int32_t a5) {\n    if (!out0 || !io1 || !io2 || !io3 || !io4) return 100;\n    if (a1 != 11) return 1;\n    if (*io1 != 101) return 2;\n    if (d1 != 1.25) return 3;\n    if (a2 != 22) return 4;\n    if (*io2 != 202) return 5;\n    if (d2 != 2.5) return 6;\n    if (a3 != 33) return 7;\n    if (*io3 != 303) return 8;\n    if (d3 != 3.75) return 9;\n    if (a4 != 44) return 10;\n    if (*io4 != 404) return 11;\n    if (a5 != 55) return 12;\n    *out0 = a1 + a2 + a3 + a4 + a5;\n    *io4 += 1;\n    return 0;\n}\n",
    );
    let c_obj = dir.join("check_keyword_spill.o");
    compile_c_object(&c_src, &c_obj);

    let mod_src = write_program_in(
        &dir,
        "c_mix.f90",
        "module c_mix\n  use iso_c_binding, only: c_int, c_double\n  implicit none\n  interface\n    function check_keyword_spill(out0, a1, io1, d1, a2, io2, d2, a3, io3, d3, a4, io4, a5) result(rc) bind(C, name='check_keyword_spill')\n      import :: c_int, c_double\n      integer(c_int), intent(out) :: out0\n      integer(c_int), value :: a1, a2, a3, a4, a5\n      integer(c_int), intent(inout) :: io1, io2, io3, io4\n      real(c_double), value :: d1, d2, d3\n      integer(c_int) :: rc\n    end function check_keyword_spill\n  end interface\nend module c_mix\n",
    );
    let main_src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use iso_c_binding, only: c_int, c_double\n  use c_mix, only: check_keyword_spill\n  implicit none\n  integer(c_int) :: rc, out0, io1, io2, io3, io4\n  io1 = 101_c_int\n  io2 = 202_c_int\n  io3 = 303_c_int\n  io4 = 404_c_int\n  rc = check_keyword_spill(a5=55_c_int, io2=io2, d2=2.5_c_double, out0=out0, a1=11_c_int, io1=io1, d1=1.25_c_double, a2=22_c_int, io3=io3, d3=3.75_c_double, a3=33_c_int, io4=io4, a4=44_c_int)\n  if (rc /= 0_c_int) error stop rc\n  if (out0 /= 165_c_int) error stop 21\n  if (io4 /= 405_c_int) error stop 22\n  print *, 'ok'\nend program\n",
    );

    let mod_obj = dir.join("c_mix.o");
    let compile_mod = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-J",
            dir.to_str().unwrap(),
            mod_src.to_str().unwrap(),
            "-o",
            mod_obj.to_str().unwrap(),
        ])
        .output()
        .expect("cross-TU bind(c) module compile failed to spawn");
    assert!(
        compile_mod.status.success(),
        "cross-TU bind(c) module compile failed: {}",
        String::from_utf8_lossy(&compile_mod.stderr)
    );

    let main_obj = dir.join("main.o");
    let compile_main = Command::new(compiler("armfortas"))
        .current_dir(&dir)
        .args([
            "-c",
            "-I",
            dir.to_str().unwrap(),
            "-J",
            dir.to_str().unwrap(),
            main_src.to_str().unwrap(),
            "-o",
            main_obj.to_str().unwrap(),
        ])
        .output()
        .expect("cross-TU bind(c) main compile failed to spawn");
    assert!(
        compile_main.status.success(),
        "cross-TU bind(c) main compile failed: {}",
        String::from_utf8_lossy(&compile_main.stderr)
    );

    let exe = dir.join("cross_tu_bindc_keyword_spills.bin");
    link_program(&[&main_obj, &mod_obj, &c_obj], &exe);

    let run = Command::new(&exe)
        .output()
        .expect("cross-TU bind(c) runtime failed");
    assert!(
        run.status.success(),
        "cross-TU bind(c) runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ok"),
        "unexpected cross-TU bind(c) output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn character_result_actual_preserves_scalar_spills_in_nested_calls() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=character_result_actual_preserves_scalar_spills_in_nested_calls count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("char_result_nested_actual");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer :: total\n  total = consume(label=render(a9=9, d2=2.5d0, a3=3, a5=5, a1=1, tag='ab', d1=1.25d0, a7=7, a2=2, a4=4, a6=6, a8=8), &\n                  a9=9, d2=2.5d0, a3=3, a5=5, a1=1, d1=1.25d0, a7=7, a2=2, a4=4, a6=6, a8=8)\n  if (total /= 53) error stop 1\n  print *, total\ncontains\n  function render(tag, a1, a2, a3, a4, a5, a6, a7, a8, a9, d1, d2) result(out)\n    character(len=*), intent(in) :: tag\n    integer, intent(in) :: a1, a2, a3, a4, a5, a6, a7, a8, a9\n    real(8), intent(in) :: d1, d2\n    character(len=16) :: out\n    write(out, '(A,I0)') trim(tag) // '=', a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 + a9 + int(d1) + int(d2)\n  end function render\n\n  integer function consume(label, a1, a2, a3, a4, a5, a6, a7, a8, a9, d1, d2) result(v)\n    character(len=*), intent(in) :: label\n    integer, intent(in) :: a1, a2, a3, a4, a5, a6, a7, a8, a9\n    real(8), intent(in) :: d1, d2\n    v = len_trim(label) + a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 + a9 + int(d1) + int(d2)\n  end function consume\nend program\n",
    );
    let exe = dir.join("char_result_nested_actual.bin");
    compile_fortran_program(&src, &exe);

    let run = Command::new(&exe)
        .output()
        .expect("character-result nested-actual runtime failed");
    assert!(
        run.status.success(),
        "character-result nested-actual runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("53"),
        "unexpected character-result nested-actual output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn hidden_result_builder_preserves_scalar_helper_spills() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=hidden_result_builder_preserves_scalar_helper_spills count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("hidden_result_builder_spills");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  character(len=32) :: out\n  out = render(a9=99, d8=8.5d0, a4=44, d2=2.5d0, tag='xy', a1=11, d5=5.25d0, a7=77, d1=1.25d0, &\n               a2=22, d9=9.25d0, a5=55, d4=4.5d0, a8=88, a3=33, d6=6.5d0, a6=66, d3=3.75d0, d7=7.75d0)\n  if (trim(out) /= 'xy=542') error stop 1\n  print *, trim(out)\ncontains\n  function render(tag, a1, a2, a3, a4, a5, a6, a7, a8, a9, d1, d2, d3, d4, d5, d6, d7, d8, d9) result(out)\n    character(len=*), intent(in) :: tag\n    integer, intent(in) :: a1, a2, a3, a4, a5, a6, a7, a8, a9\n    real(8), intent(in) :: d1, d2, d3, d4, d5, d6, d7, d8, d9\n    character(len=32) :: out\n    write(out, '(A,I0)') trim(tag) // '=', weight(a9=a9, d8=d8, a4=a4, d2=d2, a1=a1, d5=d5, a7=a7, d1=d1, a2=a2, d9=d9, a5=a5, d4=d4, a8=a8, a3=a3, d6=d6, a6=a6, d3=d3, d7=d7)\n  end function render\n\n  integer function weight(a1, a2, a3, a4, a5, a6, a7, a8, a9, d1, d2, d3, d4, d5, d6, d7, d8, d9) result(v)\n    integer, intent(in) :: a1, a2, a3, a4, a5, a6, a7, a8, a9\n    real(8), intent(in) :: d1, d2, d3, d4, d5, d6, d7, d8, d9\n    v = a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 + a9 + int(d1) + int(d2) + int(d3) + int(d4) + int(d5) + int(d6) + int(d7) + int(d8) + int(d9) + 2\n  end function weight\nend program\n",
    );
    let exe = dir.join("hidden_result_builder_spills.bin");
    compile_fortran_program(&src, &exe);

    let run = Command::new(&exe)
        .output()
        .expect("hidden-result builder runtime failed");
    assert!(
        run.status.success(),
        "hidden-result builder runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("xy=542"),
        "unexpected hidden-result builder output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn generic_character_function_dispatches_to_character_specific() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=generic_character_function_dispatches_to_character_specific count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("generic_character_dispatch");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  interface pick\n    procedure :: pick_i, pick_c\n  end interface\n  if (pick('ab') /= 2) error stop 1\n  print *, pick('ab')\ncontains\n  integer function pick_i(n)\n    integer, intent(in) :: n\n    pick_i = n + 100\n  end function pick_i\n  integer function pick_c(s)\n    character(len=*), intent(in) :: s\n    pick_c = len_trim(s)\n  end function pick_c\nend program\n",
    );
    let exe = dir.join("generic_character_dispatch.bin");
    compile_fortran_program(&src, &exe);

    let run = Command::new(&exe)
        .output()
        .expect("generic character dispatch runtime failed");
    assert!(
        run.status.success(),
        "generic character dispatch runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("2"),
        "unexpected generic character dispatch output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn generic_hidden_result_character_dispatches_to_character_specific() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=generic_hidden_result_character_dispatches_to_character_specific count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("generic_hidden_result_character_dispatch");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  character(len=8) :: out\n  interface build\n    procedure :: build_i, build_c\n  end interface\n  out = build('ab')\n  if (trim(out) /= 'ab') error stop 1\n  print *, trim(out)\ncontains\n  function build_i(n) result(out)\n    integer, intent(in) :: n\n    character(len=8) :: out\n    write(out, '(I0)') n\n  end function build_i\n  function build_c(s) result(out)\n    character(len=*), intent(in) :: s\n    character(len=8) :: out\n    out = s\n  end function build_c\nend program\n",
    );
    let exe = dir.join("generic_hidden_result_character_dispatch.bin");
    compile_fortran_program(&src, &exe);

    let run = Command::new(&exe)
        .output()
        .expect("generic hidden-result character dispatch runtime failed");
    assert!(
        run.status.success(),
        "generic hidden-result character dispatch runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("ab"),
        "unexpected generic hidden-result character dispatch output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn specific_intrinsic_procedure_targets_keep_fortran_abi_at_every_opt_level() {
    const OPT_LEVELS: [&str; 6] = ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"];

    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=calling_convention_runtime test=specific_intrinsic_procedure_targets_keep_fortran_abi_at_every_opt_level count={} reason=\"{}\"",
            OPT_LEVELS.len(),
            reason
        );
        return;
    }

    let dir = unique_dir("specific_intrinsic_procptr");
    let src = write_program_in(
        &dir,
        "main.f90",
        r#"module callback_types
  implicit none
  abstract interface
    real function real_callback(value)
      real, intent(in) :: value
    end function real_callback
    double precision function double_callback(value)
      double precision, intent(in) :: value
    end function double_callback
    integer function integer_callback(value)
      integer, intent(in) :: value
    end function integer_callback
    integer function character_callback(value)
      character(len=*), intent(in) :: value
    end function character_callback
    complex function complex_callback(value)
      complex, intent(in) :: value
    end function complex_callback
  end interface
  intrinsic :: sin
  type :: callback_holder
    procedure(real_callback), pointer, nopass :: initialized => sin
    procedure(real_callback), pointer, nopass :: rebound => null()
  end type callback_holder
end module callback_types

program p
  use callback_types
  implicit none
  intrinsic :: sin, dsin, iabs, len, conjg
  procedure(real_callback), pointer :: initialized => sin
  procedure(real_callback), pointer :: assigned
  procedure(double_callback), pointer :: double_target
  procedure(integer_callback), pointer :: integer_target
  procedure(character_callback), pointer :: character_target
  procedure(complex_callback), pointer :: complex_target
  type(callback_holder) :: holder
  complex :: result

  assigned => sin
  double_target => dsin
  integer_target => iabs
  character_target => len
  complex_target => conjg
  holder%rebound => sin

  if (abs(initialized(0.5) - sin(0.5)) > 1.0e-6) error stop 1
  if (abs(assigned(0.25) - sin(0.25)) > 1.0e-6) error stop 2
  if (abs(holder%initialized(0.75) - sin(0.75)) > 1.0e-6) error stop 3
  if (abs(holder%rebound(1.0) - sin(1.0)) > 1.0e-6) error stop 4
  if (abs(double_target(0.5d0) - dsin(0.5d0)) > 1.0d-12) error stop 5
  if (integer_target(-7) /= 7) error stop 6
  if (character_target('compiler') /= 8) error stop 7
  result = complex_target((1.5, -2.0))
  if (abs(real(result) - 1.5) > 1.0e-6) error stop 8
  if (abs(aimag(result) - 2.0) > 1.0e-6) error stop 9
  print *, 'ok'
end program p
"#,
    );

    for opt_level in OPT_LEVELS {
        let tag = opt_level.trim_start_matches('-');
        let exe = dir.join(format!("specific_intrinsic_procptr_{tag}.bin"));
        let compile = Command::new(compiler("armfortas"))
            .arg(opt_level)
            .arg(&src)
            .arg("-o")
            .arg(&exe)
            .output()
            .unwrap_or_else(|error| {
                panic!("cannot compile specific-intrinsic probe at {opt_level}: {error}")
            });
        assert!(
            compile.status.success(),
            "specific-intrinsic procedure-pointer compile failed at {}:\n{}",
            opt_level,
            String::from_utf8_lossy(&compile.stderr)
        );

        let run = Command::new(&exe).output().unwrap_or_else(|error| {
            panic!("cannot run specific-intrinsic probe at {opt_level}: {error}")
        });
        assert!(
            run.status.success(),
            "specific-intrinsic procedure-pointer runtime failed at {}: status={:?}\nstdout:\n{}\nstderr:\n{}",
            opt_level,
            run.status,
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        assert!(
            String::from_utf8_lossy(&run.stdout).contains("ok"),
            "unexpected specific-intrinsic output at {}: {}",
            opt_level,
            String::from_utf8_lossy(&run.stdout)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}
