//! x08 C-interop differential suite: armfortas-built objects linked
//! against clang-built C, BOTH call directions, on every platform
//! (macOS validates the ARM ABI, ELF hosts the SysV one). The C
//! helpers are written against the documented conventions in
//! src/ir/lower/unit.rs and the x04 classifier — never against the
//! Fortran side's observed behavior, so a convention bug cannot
//! self-validate.
//!
//! Per the x08 sprint doc, clang's presence is a hard error on ELF
//! hosts (the CI jobs install it), and a counted skip elsewhere only
//! if genuinely absent.

use std::path::PathBuf;
use std::process::Command;

fn compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary not built for this test profile")
}

fn runtime_lib() -> PathBuf {
    armfortas::testing::built_runtime_archive()
        .expect("libarmfortas_rt.a not built for this test profile")
}

/// Per-host C compiler: clang everywhere we support (cc is clang on
/// FreeBSD and macOS; the ELF CI containers install clang explicitly).
fn find_clang() -> Option<PathBuf> {
    for name in ["clang", "cc"] {
        if let Ok(out) = Command::new(name).arg("--version").output() {
            if out.status.success()
                && String::from_utf8_lossy(&out.stdout)
                    .to_lowercase()
                    .contains("clang")
            {
                return Some(PathBuf::from(name));
            }
        }
    }
    None
}

fn require_clang() -> PathBuf {
    // Hosts that cannot link natively yet (musl until x11) skip the
    // whole suite with a count, exactly like run_programs.
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=c_interop_differential test=all count=9 reason=\"{}\"",
            reason
        );
        std::process::exit(0);
    }
    match find_clang() {
        Some(p) => p,
        None => {
            let host = armfortas::target::TargetSpec::host();
            if host.object_format() == armfortas::target::ObjectFormat::Elf {
                panic!("clang is required on ELF hosts for the C-interop differential suite");
            }
            // Non-ELF host without clang: counted skip (x01 convention).
            eprintln!(
                "\nHARNESS_SKIP suite=c_interop_differential test=all count=9 reason=\"clang not found on this host\""
            );
            std::process::exit(0);
        }
    }
}

struct Workbench {
    dir: PathBuf,
    clang: PathBuf,
}

impl Workbench {
    fn new(tag: &str) -> Self {
        let dir = std::env::temp_dir().join(format!("afs_cinterop_{}_{}", tag, std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        std::fs::create_dir_all(&dir).unwrap();
        Workbench {
            dir,
            clang: require_clang(),
        }
    }

    fn fortran_obj(&self, name: &str, src: &str) -> PathBuf {
        self.fortran_obj_at(name, src, "-O0")
    }

    fn fortran_obj_at(&self, name: &str, src: &str, opt: &str) -> PathBuf {
        let f90 = self.dir.join(format!("{}.f90", name));
        let obj = self.dir.join(format!("{}.o", name));
        std::fs::write(&f90, src).unwrap();
        let r = Command::new(compiler())
            .current_dir(&self.dir)
            .args(["-c", opt])
            .arg(&f90)
            .arg("-o")
            .arg(&obj)
            .output()
            .expect("cannot run armfortas");
        assert!(
            r.status.success(),
            "Fortran compile failed:\n{}",
            String::from_utf8_lossy(&r.stderr)
        );
        obj
    }

    fn c_obj(&self, name: &str, src: &str) -> PathBuf {
        let c = self.dir.join(format!("{}.c", name));
        let obj = self.dir.join(format!("{}_c.o", name));
        std::fs::write(&c, src).unwrap();
        let r = Command::new(&self.clang)
            .args(["-c", "-O1", "-fPIC"])
            .arg(&c)
            .arg("-o")
            .arg(&obj)
            .output()
            .expect("cannot run clang");
        assert!(
            r.status.success(),
            "C compile failed:\n{}",
            String::from_utf8_lossy(&r.stderr)
        );
        obj
    }

    /// Link with the armfortas driver (it owns crt/runtime/link lines
    /// on every platform) and run, returning stdout.
    fn link_and_run(&self, name: &str, objs: &[&PathBuf]) -> String {
        let bin = self.dir.join(name);
        let mut cmd = Command::new(compiler());
        for o in objs {
            cmd.arg(o);
        }
        // The driver links the runtime automatically for .o inputs.
        let r = cmd
            .arg(runtime_lib())
            .arg("-o")
            .arg(&bin)
            .output()
            .expect("cannot run armfortas link");
        assert!(
            r.status.success(),
            "link failed:\n{}",
            String::from_utf8_lossy(&r.stderr)
        );
        let run = Command::new(&bin).output().expect("cannot run binary");
        assert!(
            run.status.success(),
            "binary exited nonzero: {:?}\nstdout: {}\nstderr: {}",
            run.status.code(),
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
        String::from_utf8_lossy(&run.stdout).trim().to_string()
    }
}

impl Drop for Workbench {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}

/// Direction 1: clang-built C calls armfortas-built BIND(C) functions.
/// Scalars by VALUE across both register files, i128, and the complex
/// returns whose eightbyte split only a differential can prove
/// (complex(4) packed in one xmm vs complex(8) in two).
#[test]
fn c_calls_fortran_scalars_i128_complex() {
    let wb = Workbench::new("c2f");
    let f = wb.fortran_obj(
        "callee",
        r#"
function add_i32(a, b) result(r) bind(c, name="add_i32")
  use iso_c_binding
  integer(c_int), value :: a, b
  integer(c_int) :: r
  r = a + b
end function add_i32

function add_i64(a, b) result(r) bind(c, name="add_i64")
  use iso_c_binding
  integer(c_int64_t), value :: a, b
  integer(c_int64_t) :: r
  r = a + b
end function add_i64

function add_i128(a, b) result(r) bind(c, name="add_i128")
  use iso_c_binding
  integer(16), value :: a, b
  integer(16) :: r
  r = a + b
end function add_i128

function add_f32(a, b) result(r) bind(c, name="add_f32")
  use iso_c_binding
  real(c_float), value :: a, b
  real(c_float) :: r
  r = a + b
end function add_f32

function add_f64(a, b) result(r) bind(c, name="add_f64")
  use iso_c_binding
  real(c_double), value :: a, b
  real(c_double) :: r
  r = a + b
end function add_f64

function make_c4(re, im) result(z) bind(c, name="make_c4")
  use iso_c_binding
  real(c_float), value :: re, im
  complex(c_float_complex) :: z
  z = cmplx(re, im)
end function make_c4

function make_c8(re, im) result(z) bind(c, name="make_c8")
  use iso_c_binding
  real(c_double), value :: re, im
  complex(c_double_complex) :: z
  z = cmplx(re, im, kind=8)
end function make_c8
"#,
    );
    let c = wb.c_obj(
        "main",
        r#"
#include <stdio.h>
#include <complex.h>
int add_i32(int, int);
long long add_i64(long long, long long);
__int128 add_i128(__int128, __int128);
float add_f32(float, float);
double add_f64(double, double);
float complex make_c4(float, float);
double complex make_c8(double, double);

int main(void) {
  printf("%d\n", add_i32(40, 2));
  printf("%lld\n", add_i64(1LL << 40, 5));
  __int128 big = ((__int128)1 << 100) + 7;
  __int128 r = add_i128(big, (__int128)35);
  /* print high and low halves to avoid 128-bit printf */
  printf("%llu %llu\n", (unsigned long long)(r >> 64), (unsigned long long)r);
  printf("%.2f\n", add_f32(1.25f, 2.25f));
  printf("%.2f\n", add_f64(4.5, 8.25));
  float complex c4 = make_c4(1.5f, -2.5f);
  double complex c8 = make_c8(3.25, 4.75);
  printf("%.2f %.2f %.2f %.2f\n", crealf(c4), cimagf(c4), creal(c8), cimag(c8));
  return 0;
}
"#,
    );
    let out = wb.link_and_run("c2f_bin", &[&c, &f]);
    let expect = "42\n1099511627781\n68719476736 42\n3.50\n12.75\n1.50 -2.50 3.25 4.75";
    assert_eq!(out, expect, "C→Fortran ABI divergence");
}

/// Direction 2: armfortas calls clang-built C — same shapes mirrored,
/// proving the caller-side conventions independently.
#[test]
fn fortran_calls_c_scalars_i128_complex() {
    let wb = Workbench::new("f2c");
    let c = wb.c_obj(
        "helpers",
        r#"
#include <complex.h>
int c_add_i32(int a, int b) { return a + b; }
long long c_add_i64(long long a, long long b) { return a + b; }
__int128 c_add_i128(__int128 a, __int128 b) { return a + b; }
float c_add_f32(float a, float b) { return a + b; }
double c_add_f64(double a, double b) { return a + b; }
float complex c_make_c4(float re, float im) { return re + im * I; }
double complex c_make_c8(double re, double im) { return re + im * I; }
"#,
    );
    let f = wb.fortran_obj(
        "main",
        r#"
program f2c
  use iso_c_binding
  implicit none
  interface
    function c_add_i32(a, b) result(r) bind(c, name="c_add_i32")
      import :: c_int
      integer(c_int), value :: a, b
      integer(c_int) :: r
    end function c_add_i32
    function c_add_i64(a, b) result(r) bind(c, name="c_add_i64")
      import :: c_int64_t
      integer(c_int64_t), value :: a, b
      integer(c_int64_t) :: r
    end function c_add_i64
    function c_add_i128(a, b) result(r) bind(c, name="c_add_i128")
      integer(16), value :: a, b
      integer(16) :: r
    end function c_add_i128
    function c_add_f32(a, b) result(r) bind(c, name="c_add_f32")
      import :: c_float
      real(c_float), value :: a, b
      real(c_float) :: r
    end function c_add_f32
    function c_add_f64(a, b) result(r) bind(c, name="c_add_f64")
      import :: c_double
      real(c_double), value :: a, b
      real(c_double) :: r
    end function c_add_f64
    function c_make_c4(re, im) result(z) bind(c, name="c_make_c4")
      import :: c_float, c_float_complex
      real(c_float), value :: re, im
      complex(c_float_complex) :: z
    end function c_make_c4
    function c_make_c8(re, im) result(z) bind(c, name="c_make_c8")
      import :: c_double, c_double_complex
      real(c_double), value :: re, im
      complex(c_double_complex) :: z
    end function c_make_c8
  end interface
  integer(16) :: big, r16
  complex(4) :: z4
  complex(8) :: z8
  print *, c_add_i32(40_c_int, 2_c_int)
  print *, c_add_i64(1099511627776_c_int64_t, 5_c_int64_t)
  big = 1267650600228229401496703205376_16 + 7_16   ! 2**100 + 7
  r16 = c_add_i128(big, 35_16)
  print *, r16
  print *, int(c_add_f32(1.25_c_float, 2.25_c_float) * 100.0)
  print *, int(c_add_f64(4.5_c_double, 8.25_c_double) * 100.0_8)
  z4 = c_make_c4(1.5_c_float, -2.5_c_float)
  z8 = c_make_c8(3.25_c_double, 4.75_c_double)
  print *, int(real(z4) * 100.0), int(aimag(z4) * 100.0)
  print *, int(real(z8) * 100.0_8), int(aimag(z8) * 100.0_8)
end program f2c
"#,
    );
    let out = wb.link_and_run("f2c_bin", &[&f, &c]);
    let normalized: Vec<String> = out
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    assert_eq!(
        normalized,
        vec![
            "42",
            "1099511627781",
            "1267650600228229401496703205418",
            "350",
            "1275",
            "150 -250",
            "325 475",
        ],
        "Fortran→C ABI divergence; raw output:\n{}",
        out
    );
}

#[test]
fn floating_point_contraction_matches_public_policy() {
    let wb = Workbench::new("fp_contract");
    let strict = wb.fortran_obj_at(
        "strict",
        r#"
function strict_muladd(a, b, c) result(r) bind(c, name="strict_muladd")
  use iso_c_binding
  real(c_double), value :: a, b, c
  real(c_double) :: r
  r = a * b + c
end function strict_muladd
"#,
        "-O2",
    );
    let fast = wb.fortran_obj_at(
        "fast",
        r#"
function fast_muladd(a, b, c) result(r) bind(c, name="fast_muladd")
  use iso_c_binding
  real(c_double), value :: a, b, c
  real(c_double) :: r
  r = a * b + c
end function fast_muladd
"#,
        "-Ofast",
    );
    let c = wb.c_obj(
        "main",
        r#"
#include <stdint.h>
#include <stdio.h>
#include <string.h>

double strict_muladd(double, double, double);
double fast_muladd(double, double, double);

static uint64_t bits(double value) {
  uint64_t result;
  memcpy(&result, &value, sizeof(result));
  return result;
}

int main(void) {
  const double a = 0x1.0000002p0;
  const double b = 0x1.ffffffcp-1;
  const double c = -1.0;
  const uint64_t strict_bits = bits(strict_muladd(a, b, c));
  const uint64_t fast_bits = bits(fast_muladd(a, b, c));
#if defined(__aarch64__)
  const uint64_t expected_fast = UINT64_C(0xbc90000000000000);
#else
  const uint64_t expected_fast = UINT64_C(0);
#endif

  if (strict_bits != UINT64_C(0) || fast_bits != expected_fast) {
    fprintf(stderr, "strict=%016llx fast=%016llx expected=%016llx\n",
            (unsigned long long)strict_bits,
            (unsigned long long)fast_bits,
            (unsigned long long)expected_fast);
    return 1;
  }
  puts("ok");
  return 0;
}
"#,
    );

    assert_eq!(
        wb.link_and_run("fp_contract_bin", &[&c, &strict, &fast]),
        "ok"
    );
}

#[test]
fn arm64_complex_value_arguments_match_clang_in_both_directions() {
    let host = armfortas::target::TargetSpec::host();
    if host.arch != armfortas::target::Arch::Arm64
        || host.object_format() != armfortas::target::ObjectFormat::MachO
    {
        eprintln!(
            "\nHARNESS_SKIP suite=c_interop_differential test=arm64_complex_value_arguments_match_clang_in_both_directions count=2 reason=\"Apple ARM64 ABI check\""
        );
        return;
    }

    let wb = Workbench::new("arm64_complex_args");
    let c = wb.c_obj(
        "helpers",
        r#"
#include <complex.h>

extern float f_take_c4(float complex);
extern double f_take_c8(double complex);
extern float f_take_c4_overflow(float, float, float, float, float, float, float,
                                float complex, float, int);
extern double f_take_c8_overflow(double, double, double, double, double, double,
                                 double, double complex, double, int);

float c_take_c4(float complex z) {
  return crealf(z) + 10.0f * cimagf(z);
}

double c_take_c8(double complex z) {
  return creal(z) + 100.0 * cimag(z);
}

float c_take_c4_overflow(float a1, float a2, float a3, float a4, float a5,
                         float a6, float a7, float complex z, float tail,
                         int marker) {
  return crealf(z) + 10.0f * cimagf(z) + 100.0f * tail + marker;
}

double c_take_c8_overflow(double a1, double a2, double a3, double a4, double a5,
                          double a6, double a7, double complex z, double tail,
                          int marker) {
  return creal(z) + 100.0 * cimag(z) + 1000.0 * tail + marker;
}

int c_call_fortran(void) {
  float r4 = f_take_c4(-1.0f + 2.0f * I);
  double r8 = f_take_c8(2.0 - 3.0 * I);
  float o4 = f_take_c4_overflow(1.0f, 1.0f, 1.0f, 1.0f, 1.0f, 1.0f, 1.0f,
                                2.0f + 3.0f * I, 4.0f, 5);
  double o8 = f_take_c8_overflow(1.0, 1.0, 1.0, 1.0, 1.0, 1.0, 1.0,
                                 2.0 + 3.0 * I, 4.0, 5);
  return r4 == 19.0f && r8 == -298.0 && o4 == 437.0f && o8 == 4307.0;
}
"#,
    );
    let f = wb.fortran_obj_at(
        "main",
        r#"
function f_take_c4(z) result(r) bind(c, name="f_take_c4")
  use iso_c_binding
  complex(c_float_complex), value :: z
  real(c_float) :: r
  r = real(z, c_float) + 10.0_c_float * aimag(z)
end function f_take_c4

function f_take_c8(z) result(r) bind(c, name="f_take_c8")
  use iso_c_binding
  complex(c_double_complex), value :: z
  real(c_double) :: r
  r = real(z, c_double) + 100.0_c_double * aimag(z)
end function f_take_c8

function f_take_c4_overflow(a1,a2,a3,a4,a5,a6,a7,z,tail,marker) result(r) &
    bind(c, name="f_take_c4_overflow")
  use iso_c_binding
  real(c_float), value :: a1,a2,a3,a4,a5,a6,a7,tail
  complex(c_float_complex), value :: z
  integer(c_int), value :: marker
  real(c_float) :: r
  r = real(z, c_float) + 10.0_c_float * aimag(z) + 100.0_c_float * tail + marker
end function f_take_c4_overflow

function f_take_c8_overflow(a1,a2,a3,a4,a5,a6,a7,z,tail,marker) result(r) &
    bind(c, name="f_take_c8_overflow")
  use iso_c_binding
  real(c_double), value :: a1,a2,a3,a4,a5,a6,a7,tail
  complex(c_double_complex), value :: z
  integer(c_int), value :: marker
  real(c_double) :: r
  r = real(z, c_double) + 100.0_c_double * aimag(z) + 1000.0_c_double * tail + marker
end function f_take_c8_overflow

program p
  use iso_c_binding
  implicit none
  interface
    function c_take_c4(z) result(r) bind(c, name="c_take_c4")
      import :: c_float, c_float_complex
      complex(c_float_complex), value :: z
      real(c_float) :: r
    end function c_take_c4
    function c_take_c8(z) result(r) bind(c, name="c_take_c8")
      import :: c_double, c_double_complex
      complex(c_double_complex), value :: z
      real(c_double) :: r
    end function c_take_c8
    function c_take_c4_overflow(a1,a2,a3,a4,a5,a6,a7,z,tail,marker) result(r) &
        bind(c, name="c_take_c4_overflow")
      import :: c_float, c_float_complex, c_int
      real(c_float), value :: a1,a2,a3,a4,a5,a6,a7,tail
      complex(c_float_complex), value :: z
      integer(c_int), value :: marker
      real(c_float) :: r
    end function c_take_c4_overflow
    function c_take_c8_overflow(a1,a2,a3,a4,a5,a6,a7,z,tail,marker) result(r) &
        bind(c, name="c_take_c8_overflow")
      import :: c_double, c_double_complex, c_int
      real(c_double), value :: a1,a2,a3,a4,a5,a6,a7,tail
      complex(c_double_complex), value :: z
      integer(c_int), value :: marker
      real(c_double) :: r
    end function c_take_c8_overflow
    function c_call_fortran() result(ok) bind(c, name="c_call_fortran")
      import :: c_int
      integer(c_int) :: ok
    end function c_call_fortran
  end interface
  if (c_take_c4(cmplx(1.25_c_float, -2.5_c_float, kind=c_float)) /= -23.75_c_float) error stop 1
  if (c_take_c8(cmplx(3.5_c_double, 4.25_c_double, kind=c_double)) /= 428.5_c_double) error stop 2
  if (c_take_c4_overflow(1.0_c_float, 1.0_c_float, 1.0_c_float, 1.0_c_float, &
                         1.0_c_float, 1.0_c_float, 1.0_c_float, &
                         cmplx(2.0_c_float, 3.0_c_float, kind=c_float), &
                         4.0_c_float, 5_c_int) /= 437.0_c_float) error stop 3
  if (c_take_c8_overflow(1.0_c_double, 1.0_c_double, 1.0_c_double, 1.0_c_double, &
                         1.0_c_double, 1.0_c_double, 1.0_c_double, &
                         cmplx(2.0_c_double, 3.0_c_double, kind=c_double), &
                         4.0_c_double, 5_c_int) /= 4307.0_c_double) error stop 4
  if (c_call_fortran() /= 1_c_int) error stop 5
  print *, 'ok'
end program p
"#,
        "-O2",
    );

    assert_eq!(wb.link_and_run("arm64_complex_args_bin", &[&c, &f]), "ok");
}

/// BIND(C) assumed-size character arrays are raw C buffers. Interop
/// code passes lengths explicitly; with six leading ints those
/// explicit lengths spill to the stack, which is the marshaling case
/// this pins. Scalar character(len=*) is intentionally excluded: that
/// form requires a C descriptor.
#[test]
fn fortran_calls_c_assumed_size_char_buffers_with_explicit_lengths() {
    let wb = Workbench::new("charlen");
    let c = wb.c_obj(
        "helpers",
        r#"
long long count_chars(int a, int b, int c, int d, int e, int f,
                      const char *s1, const char *s2,
                      long long l1, long long l2) {
  (void)a; (void)b; (void)c; (void)d; (void)e; (void)f;
  long long n = 0;
  for (long long i = 0; i < l1; i++) n += (s1[i] == 'x');
  for (long long i = 0; i < l2; i++) n += (s2[i] == 'y');
  return n;
}
"#,
    );
    let f = wb.fortran_obj(
        "main",
        r#"
program charlen
  use iso_c_binding
  implicit none
  interface
    function count_chars(a, b, c, d, e, f, s1, s2, l1, l2) result(n) bind(c, name="count_chars")
      import :: c_int, c_int64_t, c_char
      integer(c_int), value :: a, b, c, d, e, f
      character(kind=c_char), intent(in) :: s1(*), s2(*)
      integer(c_int64_t), value :: l1, l2
      integer(c_int64_t) :: n
    end function count_chars
  end interface
  print *, count_chars(1_c_int, 2_c_int, 3_c_int, 4_c_int, 5_c_int, 6_c_int, &
                       "xxoxx", "yyy", 5_c_int64_t, 3_c_int64_t)
end program charlen
"#,
    );
    let out = wb.link_and_run("charlen_bin", &[&f, &c]);
    assert_eq!(
        out.split_whitespace().next(),
        Some("7"),
        "explicit-length char interop divergence: {}",
        out
    );
}

/// The INTERNAL trailing hidden-length convention, cross-TU,
/// Fortran↔Fortran: a character(len=*) procedure in one TU called
/// from another with enough leading arguments that the appended
/// lengths land past the sixth GP register. Cross-TU knowledge flows
/// through the .amod char_len_star machinery.
#[test]
fn fortran_cross_tu_hidden_lengths_spill() {
    let wb = Workbench::new("ftnlen");
    let callee = wb.fortran_obj(
        "lenmod",
        r#"
module lenmod
  implicit none
contains
  function tally(a, b, c, d, e, f, s1, s2) result(n)
    integer(8), intent(in) :: a, b, c, d, e, f
    character(len=*), intent(in) :: s1, s2
    integer(8) :: n
    n = a + b + c + d + e + f + len(s1) * 100 + len(s2) * 10
  end function tally
end module lenmod
"#,
    );
    let caller = wb.fortran_obj(
        "main",
        r#"
program ftnlen
  use lenmod
  implicit none
  print *, tally(1_8, 2_8, 3_8, 4_8, 5_8, 6_8, "abcde", "xyz")
end program ftnlen
"#,
    );
    let out = wb.link_and_run("ftnlen_bin", &[&caller, &callee]);
    // 21 + 5*100 + 3*10 = 551
    assert_eq!(
        out.split_whitespace().next(),
        Some("551"),
        "cross-TU hidden-length divergence: {}",
        out
    );
}

/// Cross-TU mixed-opt links: caller and callee compiled at different
/// opt levels must agree on every convention (the cheapest detector
/// of caller/callee disagreement). i128 + complex + character shapes.
#[test]
fn mixed_opt_cross_tu_links() {
    let wb = Workbench::new("mixedopt");
    let callee_src = r#"
function pair_sum(a, b) result(r) bind(c, name="pair_sum")
  integer(16), value :: a, b
  integer(16) :: r
  r = a + b
end function pair_sum

function spin(re, im) result(z) bind(c, name="spin")
  use iso_c_binding
  real(c_double), value :: re, im
  complex(c_double_complex) :: z
  z = cmplx(im, re, kind=8)
end function spin
"#;
    let caller_src = r#"
program mixed
  use iso_c_binding
  implicit none
  interface
    function pair_sum(a, b) result(r) bind(c, name="pair_sum")
      integer(16), value :: a, b
      integer(16) :: r
    end function pair_sum
    function spin(re, im) result(z) bind(c, name="spin")
      import :: c_double, c_double_complex
      real(c_double), value :: re, im
      complex(c_double_complex) :: z
    end function spin
  end interface
  complex(8) :: z
  print *, pair_sum(1180591620717411303424_16, 56_16)  ! 2**70 + 56
  z = spin(3.0_c_double, 9.0_c_double)
  print *, int(real(z)), int(aimag(z))
end program mixed
"#;
    for (caller_opt, callee_opt) in [("-O0", "-O2"), ("-O2", "-O0")] {
        // ELF hosts run -O0 only in the harness, but the COMPILER can
        // build these shapes at -O2; if an opt-level gap bites, this
        // is exactly the signal the matrix exists to catch.
        let f90c = wb.dir.join("callee.f90");
        std::fs::write(&f90c, callee_src).unwrap();
        let objc = wb.dir.join(format!("callee{}.o", callee_opt));
        let r = Command::new(compiler())
            .current_dir(&wb.dir)
            .args(["-c", callee_opt])
            .arg(&f90c)
            .arg("-o")
            .arg(&objc)
            .output()
            .unwrap();
        assert!(
            r.status.success(),
            "callee at {} failed:\n{}",
            callee_opt,
            String::from_utf8_lossy(&r.stderr)
        );
        let f90m = wb.dir.join("main.f90");
        std::fs::write(&f90m, caller_src).unwrap();
        let objm = wb.dir.join(format!("main{}.o", caller_opt));
        let r = Command::new(compiler())
            .current_dir(&wb.dir)
            .args(["-c", caller_opt])
            .arg(&f90m)
            .arg("-o")
            .arg(&objm)
            .output()
            .unwrap();
        assert!(
            r.status.success(),
            "caller at {} failed:\n{}",
            caller_opt,
            String::from_utf8_lossy(&r.stderr)
        );
        let out = wb.link_and_run(
            &format!("mixed_{}_{}", &caller_opt[1..], &callee_opt[1..]),
            &[&objm, &objc],
        );
        let lines: Vec<String> = out
            .lines()
            .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
            .collect();
        assert_eq!(
            lines,
            vec!["1180591620717411303480", "9 3"],
            "mixed-opt divergence at caller={} callee={}",
            caller_opt,
            callee_opt
        );
    }
}

/// COMMON-block layout agreement across TUs: both sides declare the
/// same /shared/ block with mixed integer/real members (character
/// members are sema-rejected pending inline-byte storage, l06);
/// offsets must agree or the reader sees garbage. Both targets are
/// LP64 little-endian — divergence is a bug, never a platform
/// difference (campaign hard constraint).
#[test]
fn cross_tu_common_block_layout() {
    let wb = Workbench::new("commontu");
    let writer = wb.fortran_obj(
        "writer",
        r#"
subroutine fill_shared()
  implicit none
  integer(4) :: count
  real(8) :: weight
  integer(8) :: stamp
  common /shared/ count, weight, stamp
  count = 42
  weight = 2.5d0
  stamp = 1234567890123_8
end subroutine fill_shared
"#,
    );
    let reader = wb.fortran_obj(
        "main",
        r#"
program commontu
  implicit none
  integer(4) :: count
  real(8) :: weight
  integer(8) :: stamp
  common /shared/ count, weight, stamp
  call fill_shared()
  print *, count
  print *, int(weight * 10.0d0), stamp
end program commontu
"#,
    );
    let out = wb.link_and_run("commontu_bin", &[&reader, &writer]);
    let lines: Vec<String> = out
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    assert_eq!(
        lines,
        vec!["42", "25 1234567890123"],
        "cross-TU COMMON layout divergence:
{}",
        out
    );
}

/// COMMON array members occupy the full declared storage slot across
/// translation units. Element access, whole-array reads, and following
/// members must all agree after the linker merges the .comm symbol.
#[test]
fn cross_tu_common_array_member_layout() {
    let wb = Workbench::new("commonarraytu");
    let writer = wb.fortran_obj(
        "writer",
        r#"
subroutine fill_shared_array()
  implicit none
  integer(4) :: ia(3), tail
  common /sharedarr/ ia, tail
  ia(1) = 7
  ia(2) = 8
  ia(3) = 9
  tail = 33
end subroutine fill_shared_array
"#,
    );
    let reader = wb.fortran_obj(
        "main",
        r#"
program commonarraytu
  implicit none
  integer(4) :: nums(3), marker
  common /sharedarr/ nums, marker
  call fill_shared_array()
  print *, nums(1), nums(2), nums(3)
  print *, sum(nums), marker
end program commonarraytu
"#,
    );
    let out = wb.link_and_run("commonarraytu_bin", &[&reader, &writer]);
    let lines: Vec<String> = out
        .lines()
        .map(|l| l.split_whitespace().collect::<Vec<_>>().join(" "))
        .collect();
    assert_eq!(
        lines,
        vec!["7 8 9", "24 33"],
        "cross-TU COMMON array layout divergence:
{}",
        out
    );
}
