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
    std::env::temp_dir().join(format!("afs_regalloc_{}_{}_{}.{}", stem, pid, id, ext))
}

fn unique_dir(stem: &str) -> PathBuf {
    let dir = unique_path(stem, "dir");
    std::fs::create_dir_all(&dir).expect("cannot create regalloc test directory");
    dir
}

fn write_program_in(dir: &std::path::Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("cannot write regalloc test source");
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

fn compile_fortran_object(source: &std::path::Path, output: &std::path::Path, opt: &str) {
    let result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            source.to_str().unwrap(),
            opt,
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .expect("failed to spawn armfortas object compile");
    assert!(
        result.status.success(),
        "armfortas failed for {} at {}: {}",
        source.display(),
        opt,
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

fn compile_and_run_with_c(
    stem: &str,
    c_name: &str,
    c_text: &str,
    source_text: &str,
    opt: &str,
    expected: &str,
) {
    let dir = unique_dir(stem);
    let c_src = write_program_in(&dir, c_name, c_text);
    let c_obj = dir.join(format!("{}.o", c_name.trim_end_matches(".c")));
    compile_c_object(&c_src, &c_obj);

    let src = write_program_in(&dir, "main.f90", source_text);
    let f_obj = dir.join("main.o");
    compile_fortran_object(&src, &f_obj, opt);

    let exe = dir.join(format!("{}.bin", stem));
    link_program(&[&f_obj, &c_obj], &exe);
    let run = Command::new(&exe)
        .output()
        .expect("regalloc runtime failed to spawn");
    assert!(
        run.status.success(),
        "regalloc runtime failed for {} at {}: status={:?}\nstdout:\n{}\nstderr:\n{}",
        stem,
        opt,
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains(expected),
        "unexpected regalloc runtime output for {} at {}: {}",
        stem,
        opt,
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn gp_values_live_across_call_pressure_survive() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=regalloc_runtime test=gp_values_live_across_call_pressure_survive count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let c_text = "#include <stdint.h>\n\nint check_gp_live(int32_t a1, int32_t a2, int32_t a3, int32_t a4, int32_t a5, int32_t a6, int32_t a7, int32_t a8, int32_t a9, int32_t a10, int32_t a11, int32_t a12, int32_t a13, int32_t a14, int32_t a15, int32_t a16) {\n    return a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 + a9 + a10 + a11 + a12 + a13 + a14 + a15 + a16;\n}\n";
    let src = "program p\n  use iso_c_binding, only: c_int\n  implicit none\n  interface\n    function check_gp_live(a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15, a16) result(v) bind(C, name='check_gp_live')\n      import :: c_int\n      integer(c_int), value :: a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15, a16\n      integer(c_int) :: v\n    end function check_gp_live\n  end interface\n  integer(c_int) :: total\n  integer(c_int) :: a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15, a16\n  a1 = 1\n  a2 = 2\n  a3 = 3\n  a4 = 4\n  a5 = 5\n  a6 = 6\n  a7 = 7\n  a8 = 8\n  a9 = 9\n  a10 = 10\n  a11 = 11\n  a12 = 12\n  a13 = 13\n  a14 = 14\n  a15 = 15\n  a16 = 16\n  total = check_gp_live(a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, a13, a14, a15, a16)\n  total = total + a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 + a9 + a10 + a11 + a12 + a13 + a14 + a15 + a16\n  if (total /= 272_c_int) error stop 1\n  print *, total\nend program\n";

    for opt in ["-O0", "-O2"] {
        compile_and_run_with_c(
            "gp_live_across_call",
            "check_gp_live.c",
            c_text,
            src,
            opt,
            "272",
        );
    }
}

#[test]
fn fp_values_live_across_call_pressure_survive() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=regalloc_runtime test=fp_values_live_across_call_pressure_survive count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let c_text = "#include <stdint.h>\n\nint check_fp_live(double d1, double d2, double d3, double d4, double d5, double d6, double d7, double d8, double d9, double d10, double d11, double d12) {\n    return (int)d1 + (int)d2 + (int)d3 + (int)d4 + (int)d5 + (int)d6 + (int)d7 + (int)d8 + (int)d9 + (int)d10 + (int)d11 + (int)d12;\n}\n";
    let src = "program p\n  use iso_c_binding, only: c_int, c_double\n  implicit none\n  interface\n    function check_fp_live(d1, d2, d3, d4, d5, d6, d7, d8, d9, d10, d11, d12) result(v) bind(C, name='check_fp_live')\n      import :: c_int, c_double\n      real(c_double), value :: d1, d2, d3, d4, d5, d6, d7, d8, d9, d10, d11, d12\n      integer(c_int) :: v\n    end function check_fp_live\n  end interface\n  integer(c_int) :: total\n  real(c_double) :: d1, d2, d3, d4, d5, d6, d7, d8, d9, d10, d11, d12\n  d1 = 1.0d0\n  d2 = 2.0d0\n  d3 = 3.0d0\n  d4 = 4.0d0\n  d5 = 5.0d0\n  d6 = 6.0d0\n  d7 = 7.0d0\n  d8 = 8.0d0\n  d9 = 9.0d0\n  d10 = 10.0d0\n  d11 = 11.0d0\n  d12 = 12.0d0\n  total = check_fp_live(d1, d2, d3, d4, d5, d6, d7, d8, d9, d10, d11, d12)\n  total = total + int(d1) + int(d2) + int(d3) + int(d4) + int(d5) + int(d6) + int(d7) + int(d8) + int(d9) + int(d10) + int(d11) + int(d12)\n  if (total /= 156_c_int) error stop 1\n  print *, total\nend program\n";

    for opt in ["-O0", "-O2"] {
        compile_and_run_with_c(
            "fp_live_across_call",
            "check_fp_live.c",
            c_text,
            src,
            opt,
            "156",
        );
    }
}

#[test]
fn mixed_gp_fp_values_live_across_nested_call_pressure_survive() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=regalloc_runtime test=mixed_gp_fp_values_live_across_nested_call_pressure_survive count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let c_text = "#include <stdint.h>\n\nint check_mixed_live(int32_t a1, int32_t a2, int32_t a3, int32_t a4, int32_t a5, int32_t a6, int32_t a7, int32_t a8, int32_t a9, int32_t a10, int32_t a11, int32_t a12, double d1, double d2, double d3, double d4, double d5, double d6, double d7, double d8, double d9, double d10) {\n    return a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 + a9 + a10 + a11 + a12 + (int)d1 + (int)d2 + (int)d3 + (int)d4 + (int)d5 + (int)d6 + (int)d7 + (int)d8 + (int)d9 + (int)d10;\n}\n";
    let src = "program p\n  use iso_c_binding, only: c_int, c_double\n  implicit none\n  interface\n    function check_mixed_live(a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, d1, d2, d3, d4, d5, d6, d7, d8, d9, d10) result(v) bind(C, name='check_mixed_live')\n      import :: c_int, c_double\n      integer(c_int), value :: a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12\n      real(c_double), value :: d1, d2, d3, d4, d5, d6, d7, d8, d9, d10\n      integer(c_int) :: v\n    end function check_mixed_live\n  end interface\n  integer(c_int) :: total\n  integer(c_int) :: a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12\n  real(c_double) :: d1, d2, d3, d4, d5, d6, d7, d8, d9, d10\n  a1 = 1\n  a2 = 2\n  a3 = 3\n  a4 = 4\n  a5 = 5\n  a6 = 6\n  a7 = 7\n  a8 = 8\n  a9 = 9\n  a10 = 10\n  a11 = 11\n  a12 = 12\n  d1 = 1.0d0\n  d2 = 2.0d0\n  d3 = 3.0d0\n  d4 = 4.0d0\n  d5 = 5.0d0\n  d6 = 6.0d0\n  d7 = 7.0d0\n  d8 = 8.0d0\n  d9 = 9.0d0\n  d10 = 10.0d0\n  total = check_mixed_live(a1, a2, a3, a4, a5, a6, a7, a8, a9, a10, a11, a12, d1, d2, d3, d4, d5, d6, d7, d8, d9, d10)\n  total = total + a1 + a2 + a3 + a4 + a5 + a6 + a7 + a8 + a9 + a10 + a11 + a12 + int(d1) + int(d2) + int(d3) + int(d4) + int(d5) + int(d6) + int(d7) + int(d8) + int(d9) + int(d10)\n  if (total /= 266_c_int) error stop 1\n  print *, total\nend program\n";

    for opt in ["-O0", "-O2"] {
        compile_and_run_with_c(
            "mixed_live_across_call",
            "check_mixed_live.c",
            c_text,
            src,
            opt,
            "266",
        );
    }
}

#[test]
fn f32_stack_args_pack_without_8_byte_gaps() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=regalloc_runtime test=f32_stack_args_pack_without_8_byte_gaps count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let c_text = "#include <stdint.h>\n\nint check_f32_stack(float a1, float a2, float a3, float a4, float a5, float a6, float a7, float a8, float a9, float a10) {\n    return (int)a9 + (int)a10;\n}\n";
    let src = "program p\n  use iso_c_binding, only: c_int, c_float\n  implicit none\n  interface\n    function check_f32_stack(a1, a2, a3, a4, a5, a6, a7, a8, a9, a10) result(v) bind(C, name='check_f32_stack')\n      import :: c_int, c_float\n      real(c_float), value :: a1, a2, a3, a4, a5, a6, a7, a8, a9, a10\n      integer(c_int) :: v\n    end function check_f32_stack\n  end interface\n  integer(c_int) :: total\n  total = check_f32_stack(1.0_c_float, 2.0_c_float, 3.0_c_float, 4.0_c_float, 5.0_c_float, 6.0_c_float, 7.0_c_float, 8.0_c_float, 9.0_c_float, 10.0_c_float)\n  if (total /= 19_c_int) error stop 1\n  print *, total\nend program\n";

    for opt in ["-O0", "-O2"] {
        compile_and_run_with_c(
            "f32_stack_dense",
            "check_f32_stack.c",
            c_text,
            src,
            opt,
            "19",
        );
    }
}
