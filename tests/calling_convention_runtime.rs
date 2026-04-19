use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

fn compiler(name: &str) -> PathBuf {
    if let Some(path) = std::env::var_os(format!("CARGO_BIN_EXE_{}", name)) {
        return PathBuf::from(path);
    }
    let candidate = PathBuf::from("target/debug").join(name);
    if candidate.exists() {
        return std::fs::canonicalize(candidate).expect("cannot canonicalize debug compiler path");
    }
    let candidate = PathBuf::from("target/release").join(name);
    if candidate.exists() {
        return std::fs::canonicalize(candidate)
            .expect("cannot canonicalize release compiler path");
    }
    panic!(
        "compiler binary '{}' not built — run `cargo build --bins` first",
        name
    );
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
            "-arch",
            "arm64",
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
fn bind_c_mixed_gp_fp_value_args_match_c_peer() {
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
fn contained_hidden_result_optional_gap_preserves_host_and_char_ordering() {
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
