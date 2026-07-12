use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

fn compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("compiler binary 'armfortas' not built for this test profile")
}

fn unique_dir() -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!(
        "afs_x86_complex_value_{}_{}",
        std::process::id(),
        id
    ));
    std::fs::create_dir_all(&dir).expect("cannot create complex-value test directory");
    dir
}

fn write_source(dir: &Path, name: &str, source: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, source).expect("cannot write complex-value test source");
    path
}

fn compile_c(source: &Path, output: &Path) {
    let result = Command::new("clang")
        .args(["-fPIC", "-c"])
        .arg(source)
        .arg("-o")
        .arg(output)
        .output()
        .expect("failed to spawn clang");
    assert!(
        result.status.success(),
        "clang failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn compile_fortran(source: &Path, output: &Path, opt_level: &str) {
    let result = Command::new(compiler())
        .args(["-c", opt_level])
        .arg(source)
        .arg("-o")
        .arg(output)
        .output()
        .expect("failed to spawn armfortas");
    assert!(
        result.status.success(),
        "armfortas failed at {opt_level}: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

fn link(c_object: &Path, fortran_object: &Path, output: &Path) {
    let result = Command::new(compiler())
        .arg(c_object)
        .arg(fortran_object)
        .arg("-o")
        .arg(output)
        .output()
        .expect("failed to spawn armfortas linker driver");
    assert!(
        result.status.success(),
        "link failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
}

#[test]
fn bind_c_complex_f32_value_arguments_match_packed_xmm_abi() {
    const OPT_LEVELS: [&str; 6] = ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"];

    let host = armfortas::target::TargetSpec::host();
    if host.arch != armfortas::target::Arch::X86_64
        || host.object_format() != armfortas::target::ObjectFormat::Elf
    {
        eprintln!(
            "\nHARNESS_SKIP suite=x86_complex_value_runtime test=bind_c_complex_f32_value_arguments_match_packed_xmm_abi count={} reason=\"x86_64 ELF ABI check\"",
            OPT_LEVELS.len()
        );
        return;
    }
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=x86_complex_value_runtime test=bind_c_complex_f32_value_arguments_match_packed_xmm_abi count={} reason=\"{}\"",
            OPT_LEVELS.len(),
            reason
        );
        return;
    }

    let dir = unique_dir();
    let c_source = write_source(
        &dir,
        "main.c",
        r#"#include <complex.h>

float f_take_c4(float complex);
float f_take_c4_xmm7(double, double, double, double, double, double, double,
                     float complex, float, int);
float f_take_c4_stack(double, double, double, double, double, double, double,
                      double, float complex, float, int);
int f_check_optional_c4(void);

int main(void) {
    if (f_take_c4(-1.0f + 2.0f * I) != 19.0f) return 1;
    if (f_take_c4_xmm7(1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0,
                       2.0f + 3.0f * I, 4.0f, 5) != 437.0f) return 2;
    if (f_take_c4_stack(1.0, 2.0, 3.0, 4.0, 5.0, 6.0, 7.0, 8.0,
                        -2.0f + 5.0f * I, 6.0f, 7) != 655.0f) return 3;
    if (f_check_optional_c4() != 1) return 4;
    return 0;
}
"#,
    );
    let c_object = dir.join("main.o");
    compile_c(&c_source, &c_object);

    let fortran_source = write_source(
        &dir,
        "callee.f90",
        r#"module optional_c4_mod
  use iso_c_binding
contains
  integer(c_int) function c4_state(z)
    complex(c_float_complex), value, optional :: z
    if (.not. present(z)) then
      c4_state = 1_c_int
    else if (real(z, c_float) == 1.5_c_float .and. &
             aimag(z) == -2.25_c_float) then
      c4_state = 2_c_int
    else
      c4_state = -1_c_int
    end if
  end function c4_state

  integer(c_int) function c4_forward(z)
    complex(c_float_complex), value, optional :: z
    c4_forward = c4_state(z)
  end function c4_forward
end module optional_c4_mod

function f_check_optional_c4() result(ok) bind(c, name="f_check_optional_c4")
  use iso_c_binding
  use optional_c4_mod
  integer(c_int) :: ok
  ok = 0_c_int
  if (c4_state() /= 1_c_int) return
  if (c4_forward() /= 1_c_int) return
  if (c4_state(cmplx(1.5_c_float, -2.25_c_float, kind=c_float)) /= 2_c_int) return
  if (c4_forward(cmplx(1.5_c_float, -2.25_c_float, kind=c_float)) /= 2_c_int) return
  ok = 1_c_int
end function f_check_optional_c4

function f_take_c4(z) result(r) bind(c, name="f_take_c4")
  use iso_c_binding
  complex(c_float_complex), value :: z
  real(c_float) :: r
  r = real(z, c_float) + 10.0_c_float * aimag(z)
end function f_take_c4

function f_take_c4_xmm7(a1,a2,a3,a4,a5,a6,a7,z,tail,marker) result(r) &
    bind(c, name="f_take_c4_xmm7")
  use iso_c_binding
  real(c_double), value :: a1,a2,a3,a4,a5,a6,a7
  complex(c_float_complex), value :: z
  real(c_float), value :: tail
  integer(c_int), value :: marker
  real(c_float) :: r
  r = real(z, c_float) + 10.0_c_float * aimag(z) + 100.0_c_float * tail + marker
end function f_take_c4_xmm7

function f_take_c4_stack(a1,a2,a3,a4,a5,a6,a7,a8,z,tail,marker) result(r) &
    bind(c, name="f_take_c4_stack")
  use iso_c_binding
  real(c_double), value :: a1,a2,a3,a4,a5,a6,a7,a8
  complex(c_float_complex), value :: z
  real(c_float), value :: tail
  integer(c_int), value :: marker
  real(c_float) :: r
  r = real(z, c_float) + 10.0_c_float * aimag(z) + 100.0_c_float * tail + marker
end function f_take_c4_stack
"#,
    );

    for opt_level in OPT_LEVELS {
        let tag = opt_level.trim_start_matches('-').to_ascii_lowercase();
        let fortran_object = dir.join(format!("callee_{tag}.o"));
        let executable = dir.join(format!("complex_value_{tag}"));
        compile_fortran(&fortran_source, &fortran_object, opt_level);
        link(&c_object, &fortran_object, &executable);
        let status = Command::new(&executable)
            .status()
            .expect("failed to execute complex-value ABI test");
        assert!(
            status.success(),
            "complex-value ABI test failed at {opt_level}"
        );
    }

    std::fs::remove_dir_all(&dir).expect("cannot remove complex-value test directory");
}
