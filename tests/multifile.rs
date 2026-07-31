//! Cross-TU (multi-file) compilation tests.
//!
//! Each test compiles a module .f90 and a consumer .f90 separately
//! with `-c`, links the .o files with the runtime, runs the binary,
//! and checks the output.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn unique_dir() -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("afs_multifile_{}_{}", std::process::id(), id));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn find_compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary not built for this test profile")
}

fn find_runtime() -> PathBuf {
    armfortas::testing::built_runtime_archive()
        .expect("libarmfortas_rt.a not built for this test profile")
}

/// Compile a .f90 file with -c, producing .o and optionally .amod.
fn compile_file(compiler: &Path, source: &Path, output: &Path, search_dir: Option<&Path>) {
    compile_file_flags(compiler, source, output, search_dir, &[]);
}

fn compile_file_flags(
    compiler: &Path,
    source: &Path,
    output: &Path,
    search_dir: Option<&Path>,
    flags: &[&str],
) {
    let mut cmd = Command::new(compiler);
    if let Some(parent) = source.parent() {
        cmd.current_dir(parent);
    }
    cmd.args(flags);
    cmd.args([
        source.to_str().unwrap(),
        "-c",
        "-o",
        output.to_str().unwrap(),
    ]);
    if let Some(dir) = search_dir {
        cmd.arg(format!("-I{}", dir.display()));
    }
    let result = cmd.output().expect("compiler launch failed");
    assert!(
        result.status.success(),
        "compile {} failed:\n{}",
        source.display(),
        String::from_utf8_lossy(&result.stderr)
    );
}

/// Link .o files into a binary.
fn link_files(objects: &[&Path], output: &Path) {
    // Link through the compiler binary: the driver owns crt discovery,
    // runtime location, and the per-format link line on every platform
    // (the old inline ld invocation was Mach-O-only).
    let compiler = find_compiler();
    let runtime = find_runtime();
    let mut cmd = Command::new(&compiler);
    for o in objects {
        cmd.arg(o);
    }
    let result = cmd
        .arg(&runtime)
        .arg("-o")
        .arg(output)
        .output()
        .expect("compiler launch failed for link");
    assert!(
        result.status.success(),
        "link failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
}

/// Run a binary and return its stdout.
fn run_binary(binary: &Path) -> String {
    let result = Command::new(binary).output().expect("binary launch failed");
    assert!(
        result.status.success(),
        "{} exited with {:?}\nstderr: {}",
        binary.display(),
        result.status.code(),
        String::from_utf8_lossy(&result.stderr)
    );
    String::from_utf8_lossy(&result.stdout).into_owned()
}

fn undefined_symbols(path: &Path) -> Vec<String> {
    let out = Command::new("nm")
        .args(["-u", "-j", path.to_str().unwrap()])
        .output()
        .expect("failed to spawn nm");
    assert!(
        out.status.success(),
        "nm failed for {}: {}",
        path.display(),
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8_lossy(&out.stdout)
        .lines()
        .map(str::trim)
        .filter(|line| !line.is_empty())
        .map(ToOwned::to_owned)
        .collect()
}

/// Full multi-file test: write sources, compile, link, run, check.
fn multifile_test(mod_source: &str, main_source: &str, expected_substring: &str) {
    multifile_test_flags(mod_source, main_source, expected_substring, &[]);
}

fn multifile_test_flags(
    mod_source: &str,
    main_source: &str,
    expected_substring: &str,
    flags: &[&str],
) {
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("mod.f90");
    let main_f90 = dir.join("main.f90");
    let mod_o = dir.join("mod.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("test_bin");

    std::fs::write(&mod_f90, mod_source).unwrap();
    std::fs::write(&main_f90, main_source).unwrap();

    compile_file_flags(&compiler, &mod_f90, &mod_o, None, flags);
    compile_file_flags(&compiler, &main_f90, &main_o, Some(&dir), flags);
    link_files(&[&mod_o, &main_o], &binary);
    let output = run_binary(&binary);

    assert!(
        output_contains_expected(&output, expected_substring),
        "expected '{}' in output, got:\n{}",
        expected_substring,
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

fn output_contains_expected(output: &str, expected: &str) -> bool {
    if output.contains(expected) {
        return true;
    }
    let expected_fields: Vec<_> = expected.split_whitespace().collect();
    if expected_fields.len() <= 1 {
        return false;
    }
    let output_fields: Vec<_> = output.split_whitespace().collect();
    output_fields
        .windows(expected_fields.len())
        .any(|window| window == expected_fields.as_slice())
}

// ---- Tests ----

#[test]
fn volatile_module_variable_survives_amod_round_trip() {
    let compiler = find_compiler();
    let dir = unique_dir();
    let module_source = dir.join("volatile_provider.f90");
    let module_object = dir.join("volatile_provider.o");
    let consumer_source = dir.join("volatile_consumer.f90");

    std::fs::write(
        &module_source,
        "module volatile_provider\n  implicit none\n  integer, volatile :: watched\nend module volatile_provider\n",
    )
    .unwrap();
    std::fs::write(
        &consumer_source,
        "subroutine observe_volatile()\n  use volatile_provider, only: watched\n  implicit none\n  integer :: sink\n\n  sink = watched\n  watched = sink + 1\nend subroutine observe_volatile\n",
    )
    .unwrap();

    compile_file(&compiler, &module_source, &module_object, None);
    let amod = std::fs::read_to_string(dir.join("volatile_provider.amod"))
        .expect("volatile provider did not emit its module interface");
    assert!(
        amod.lines()
            .any(|line| line.starts_with("@var watched :") && line.contains("volatile")),
        "VOLATILE module-variable metadata must survive serialization:\n{amod}"
    );

    for optimization in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        let ir = dir.join(format!(
            "volatile_consumer_{}.ir",
            optimization.trim_start_matches('-')
        ));
        let emit = Command::new(&compiler)
            .current_dir(&dir)
            .args([
                optimization,
                "--emit-ir",
                consumer_source.to_str().unwrap(),
                "-o",
                ir.to_str().unwrap(),
            ])
            .arg(format!("-I{}", dir.display()))
            .output()
            .expect("volatile consumer IR emission failed to spawn");
        assert!(
            emit.status.success(),
            "{optimization}: volatile consumer IR emission failed: {}",
            String::from_utf8_lossy(&emit.stderr)
        );
        let ir_text =
            std::fs::read_to_string(&ir).expect("cannot read volatile consumer IR output");
        assert!(
            ir_text.contains("volatile_load") && ir_text.contains("volatile_store"),
            "{optimization}: imported VOLATILE storage must retain both observable accesses:\n{ir_text}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn serialized_intrinsic_use_keeps_provider_nature() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=serialized_intrinsic_use_keeps_provider_nature count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let compiler = find_compiler();
    let dir = unique_dir();
    let facade_f90 = dir.join("facade.f90");
    let consumer_f90 = dir.join("consumer.f90");
    let facade_o = dir.join("facade.o");
    let consumer_o = dir.join("consumer.o");
    let binary = dir.join("test_bin");

    std::fs::write(
        &facade_f90,
        "module facade\n  use, intrinsic :: iso_fortran_env\nend module facade\n",
    )
    .unwrap();
    std::fs::write(
        &consumer_f90,
        "module iso_fortran_env\n  integer, parameter :: int8 = 4\nend module iso_fortran_env\nprogram p\n  use facade, only: int8\n  if (int8 /= 1) error stop 27\n  print *, 'ok'\nend program p\n",
    )
    .unwrap();

    compile_file(&compiler, &facade_f90, &facade_o, None);
    compile_file(&compiler, &consumer_f90, &consumer_o, Some(&dir));
    link_files(&[&facade_o, &consumer_o], &binary);
    let output = run_binary(&binary);
    assert!(output.contains("ok"), "unexpected output:\n{output}");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn combined_compile_rejects_duplicate_module_definitions() {
    let compiler = find_compiler();
    let dir = unique_dir();
    std::fs::write(
        dir.join("first.f90"),
        "module shared_name\nend module shared_name\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("second.f90"),
        "module ShArEd_NaMe\nend module ShArEd_NaMe\n",
    )
    .unwrap();

    let result = Command::new(&compiler)
        .current_dir(&dir)
        .args(["-c", "first.f90", "second.f90"])
        .output()
        .expect("compiler launch failed");
    assert!(!result.status.success(), "duplicate modules were accepted");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("duplicate module definition 'shared_name'")
            && stderr.contains("first.f90")
            && stderr.contains("second.f90"),
        "unexpected duplicate-module diagnostic:\n{stderr}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn combined_compile_accepts_prefixed_module_procedure_interfaces() {
    let compiler = find_compiler();
    let dir = unique_dir();
    std::fs::write(
        dir.join("first.f90"),
        "module first_contract\n  interface\n    module pure function first_value()\n      integer :: first_value\n    end function first_value\n  end interface\nend module first_contract\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("second.f90"),
        "module second_contract\n  interface\n    module pure function second_value()\n      integer :: second_value\n    end function second_value\n  end interface\nend module second_contract\n",
    )
    .unwrap();

    let result = Command::new(&compiler)
        .current_dir(&dir)
        .args(["-c", "first.f90", "second.f90"])
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "prefixed module procedures were treated as module definitions:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn combined_compile_ignores_fixed_comment_and_hollerith_dependencies() {
    let compiler = find_compiler();
    let dir = unique_dir();
    std::fs::write(
        dir.join("producer.f"),
        "      MODULE FIXED_SOURCE\nC COMMENT; USE COMMENT_DEP\n      CONTAINS\n      SUBROUTINE SHOW(I)\n      INTEGER I\n  100 FORMAT(11H;USE HOLLER,I2)\n      PRINT 100,I\n      END\n      END\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("consumers.f90"),
        "module comment_dep\n  use fixed_source\nend module comment_dep\nmodule holler\n  use fixed_source\nend module holler\n",
    )
    .unwrap();

    let result = Command::new(&compiler)
        .current_dir(&dir)
        .args(["-c", "consumers.f90", "producer.f"])
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "fixed comments or Hollerith text created a false dependency cycle:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn combined_compile_preserves_target_and_preprocessor_options() {
    let compiler = find_compiler();
    let dir = unique_dir();
    for name in ["one.F90", "two.F90"] {
        std::fs::write(
            dir.join(name),
            format!(
                "#ifndef AUDIT_FLAG\n#error AUDIT_FLAG was dropped\n#endif\nsubroutine {}()\nend subroutine\n",
                name.trim_end_matches(".F90")
            ),
        )
        .unwrap();
    }

    let result = Command::new(&compiler)
        .current_dir(&dir)
        .args([
            "--target",
            "x86_64-freebsd",
            "-DAUDIT_FLAG",
            "-c",
            "one.F90",
            "two.F90",
        ])
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "combined compile failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );

    for name in ["one.o", "two.o"] {
        let bytes = std::fs::read(dir.join(name)).expect("combined compile omitted object");
        assert_eq!(&bytes[..4], b"\x7fELF", "{name} is not an ELF object");
        assert_eq!(bytes[7], 9, "{name} did not preserve the FreeBSD target");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn combined_compile_ignores_macro_disabled_dependency_edges() {
    let compiler = find_compiler();
    let dir = unique_dir();
    std::fs::write(
        dir.join("a.F90"),
        "#warning dependency scan should stay quiet\nmodule a\n#ifndef ACTIVE_BUILD\n  use b\n#endif\n#ifndef __x86_64__\n  use b\n#endif\n#ifndef __FreeBSD__\n  use b\n#endif\nend module a\n",
    )
    .unwrap();
    std::fs::write(dir.join("b.F90"), "module b\n  use a\nend module b\n").unwrap();

    let result = Command::new(&compiler)
        .current_dir(&dir)
        .args([
            "--target",
            "x86_64-freebsd",
            "-DACTIVE_BUILD",
            "-c",
            "b.F90",
            "a.F90",
        ])
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "inactive branch created a dependency cycle:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(dir.join("a.o").is_file());
    assert!(dir.join("b.o").is_file());
    assert_eq!(
        String::from_utf8_lossy(&result.stderr)
            .matches("#warning dependency scan should stay quiet")
            .count(),
        1,
        "dependency preprocessing duplicated user-facing warnings"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn combined_compile_orders_dependencies_from_preprocessor_includes() {
    let compiler = find_compiler();
    let dir = unique_dir();
    let includes = dir.join("includes");
    std::fs::create_dir_all(&includes).unwrap();
    std::fs::write(includes.join("dependency.inc"), "  use b\n").unwrap();
    std::fs::write(
        dir.join("a.F90"),
        "module a\n#include \"dependency.inc\"\nend module a\n",
    )
    .unwrap();
    std::fs::write(dir.join("b.F90"), "module b\nend module b\n").unwrap();

    let result = Command::new(&compiler)
        .current_dir(&dir)
        .args(["-Iincludes", "-c", "a.F90", "b.F90"])
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "included dependency did not affect compilation order:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(dir.join("a.o").is_file());
    assert!(dir.join("b.o").is_file());

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn combined_compile_orders_semicolon_separated_uses() {
    let compiler = find_compiler();
    let dir = unique_dir();
    std::fs::write(
        dir.join("consumer.f90"),
        "module consumer; use first_provider; use second_provider; end module consumer\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("first.f90"),
        "module first_provider\nend module first_provider\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("second.f90"),
        "module second_provider\nend module second_provider\n",
    )
    .unwrap();

    let result = Command::new(&compiler)
        .current_dir(&dir)
        .args(["-c", "consumer.f90", "second.f90", "first.f90"])
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "semicolon USE dependencies were not ordered:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for object in ["consumer.o", "first.o", "second.o"] {
        assert!(dir.join(object).is_file(), "missing {object}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn combined_compile_orders_unqualified_same_named_intrinsic_module() {
    let compiler = find_compiler();
    let dir = unique_dir();
    std::fs::write(
        dir.join("consumer.f90"),
        "program consumer\n  use iso_fortran_env, only: shadow_value\n  if (shadow_value /= 41) error stop\nend program consumer\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("provider.f90"),
        "module iso_fortran_env\n  integer, parameter :: shadow_value = 41\nend module iso_fortran_env\n",
    )
    .unwrap();

    let result = Command::new(&compiler)
        .current_dir(&dir)
        .args(["-c", "consumer.f90", "provider.f90"])
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "unqualified same-named module dependency was not ordered:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    for object in ["consumer.o", "provider.o"] {
        assert!(dir.join(object).is_file(), "missing {object}");
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn combined_link_keeps_equal_basename_sources_distinct() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=combined_link_keeps_equal_basename_sources_distinct count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let compiler = find_compiler();
    let dir = unique_dir();
    std::fs::create_dir_all(dir.join("a")).unwrap();
    std::fs::create_dir_all(dir.join("b")).unwrap();
    std::fs::write(
        dir.join("a/unit.f90"),
        "subroutine alpha()\n  print *, 1\nend subroutine\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("b/unit.f90"),
        "subroutine beta()\n  print *, 2\nend subroutine\n",
    )
    .unwrap();
    std::fs::write(
        dir.join("main.f90"),
        "program p\n  call alpha()\n  call beta()\nend program\n",
    )
    .unwrap();

    let binary = dir.join("same_basename");
    let result = Command::new(&compiler)
        .current_dir(&dir)
        .args(["a/unit.f90", "b/unit.f90", "main.f90", "-o"])
        .arg(&binary)
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "combined link failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = run_binary(&binary);
    assert!(
        output_contains_expected(&output, "1 2"),
        "equal-basename sources produced the wrong program:\n{output}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn basic_module_variable_and_subroutine() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=basic_module_variable_and_subroutine count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    multifile_test(
        "module m\n  implicit none\n  integer :: counter = 0\ncontains\n  subroutine bump()\n    counter = counter + 1\n  end subroutine\n  integer function get() result(r)\n    r = counter\n  end function\nend module\n",
        "program p\n  use m\n  call bump(); call bump(); call bump()\n  print *, get()\nend program\n",
        "3",
    );
}

#[test]
fn module_with_allocatable_array() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=module_with_allocatable_array count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    multifile_test(
        "module arr_mod\n  implicit none\n  integer, allocatable :: buf(:)\ncontains\n  subroutine init()\n    allocate(buf(3))\n    buf(1) = 10; buf(2) = 20; buf(3) = 30\n  end subroutine\nend module\n",
        "program p\n  use arr_mod\n  call init()\n  print *, buf(1), buf(2), buf(3)\nend program\n",
        "10 20 30",
    );
}

// Regression: gfortran/flang accept Fortran sources and prebuilt objects
// mixed on one command line, e.g. `fc main.f90 mod.o -o prog`. fortsh's
// unit-test rules use exactly this shape (`fc test.f90 build/foo.o -o test`).
// armfortas used to reject it ("mixing Fortran sources with prebuilt
// object/archive inputs is not yet supported"); now it compiles the sources
// and links them with the artifacts in command order.
#[test]
fn mixed_source_and_object_in_one_invocation() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=mixed_source_and_object_in_one_invocation count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("mixmod.f90");
    let main_f90 = dir.join("mixmain.f90");
    let mod_o = dir.join("mixmod.o");
    let binary = dir.join("mixbin");

    std::fs::write(
        &mod_f90,
        "module mixmod\n  implicit none\ncontains\n  integer function answer() result(r)\n    r = 42\n  end function\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use mixmod\n  print *, answer()\nend program\n",
    )
    .unwrap();

    // Compile the module to an object up front.
    compile_file(&compiler, &mod_f90, &mod_o, None);

    // The fix under test: one invocation with a SOURCE and an OBJECT.
    let result = Command::new(&compiler)
        .arg(&main_f90)
        .arg(&mod_o)
        .arg("-o")
        .arg(&binary)
        .arg(format!("-I{}", dir.display()))
        .output()
        .expect("compiler launch failed for mixed source+object");
    assert!(
        result.status.success(),
        "mixed source+object invocation failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );

    let output = run_binary(&binary);
    assert!(
        output.contains("42"),
        "expected '42' in output, got:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn truncated_amod_is_rejected_loudly() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=truncated_amod_is_rejected_loudly count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let provider_f90 = dir.join("provider.f90");
    let provider_o = dir.join("provider.o");
    let consumer_f90 = dir.join("consumer.f90");
    let consumer_o = dir.join("consumer.o");

    std::fs::write(
        &provider_f90,
        "module provider\n  implicit none\n  integer, parameter :: answer = 41\nend module\n",
    )
    .unwrap();
    compile_file(&compiler, &provider_f90, &provider_o, None);

    let amod_path = dir.join("provider.amod");
    let mut amod_text = std::fs::read_to_string(&amod_path).expect("missing provider.amod");
    let truncate_at = amod_text
        .find("@param answer")
        .expect("provider.amod should contain answer parameter");
    amod_text.truncate(truncate_at);
    std::fs::write(&amod_path, amod_text).expect("cannot corrupt provider.amod");

    std::fs::write(
        &consumer_f90,
        "program p\n  use provider\n  implicit none\n  print *, answer\nend program\n",
    )
    .unwrap();
    let result = Command::new(&compiler)
        .current_dir(&dir)
        .arg(&consumer_f90)
        .arg("-c")
        .arg("-o")
        .arg(&consumer_o)
        .arg(format!("-I{}", dir.display()))
        .output()
        .expect("consumer compile failed to spawn");
    assert!(
        !result.status.success(),
        "consumer unexpectedly accepted a corrupt .amod"
    );
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        stderr.contains("corrupt .amod file"),
        "expected corrupt .amod diagnostic, got:\n{}",
        stderr
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn amod_omits_stale_abi_stamp() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=amod_omits_stale_abi_stamp count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let module_f90 = dir.join("target_stamp.f90");

    std::fs::write(
        &module_f90,
        "module target_stamp\n  implicit none\ncontains\n  subroutine consume(text)\n    character(*), intent(in) :: text\n    if (len(text) < 0) error stop\n  end subroutine\nend module\n",
    )
    .unwrap();

    let mut artifacts = Vec::new();
    for target in ["x86_64-linux-musl", "arm64-macos"] {
        let target_dir = dir.join(target);
        std::fs::create_dir_all(&target_dir).unwrap();
        let compile = Command::new(&compiler)
            .current_dir(&dir)
            .args(["-c", "--target", target, "-J"])
            .arg(&target_dir)
            .arg(&module_f90)
            .args(["-o"])
            .arg(target_dir.join("target_stamp.o"))
            .output()
            .expect("cross-target module compile failed to spawn");
        assert!(
            compile.status.success(),
            "{target} module compile failed:\n{}",
            String::from_utf8_lossy(&compile.stderr)
        );
        artifacts.push(
            std::fs::read_to_string(target_dir.join("target_stamp.amod"))
                .expect("missing target_stamp.amod"),
        );
    }

    assert_eq!(
        artifacts[0], artifacts[1],
        ".amod procedure metadata should be destination-independent"
    );
    let amod = &artifacts[0];
    assert!(
        !amod.lines().any(|line| line.starts_with("# abi:")),
        ".amod should not stamp a non-authoritative ABI line:\n{}",
        amod
    );
    assert!(
        !amod.contains("cc=aapcs64") && !amod.contains("@abi pass="),
        ".amod should not stamp target-specific procedure ABI annotations:\n{}",
        amod
    );
    assert!(
        amod.contains("@arg text@len : integer(8)"),
        ".amod must retain target-independent hidden-length metadata:\n{}",
        amod
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn module_with_derived_type() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=module_with_derived_type count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    multifile_test(
        "module dt_mod\n  implicit none\n  type :: point\n    real :: x, y\n  end type\ncontains\n  subroutine set_pt(p, a, b)\n    type(point), intent(out) :: p\n    real, intent(in) :: a, b\n    p%x = a; p%y = b\n  end subroutine\nend module\n",
        "program p\n  use dt_mod\n  type(point) :: pt\n  call set_pt(pt, 1.5, 2.5)\n  print *, pt%x, pt%y\nend program\n",
        "1.5",
    );
}

#[test]
fn module_parameter_constants() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=module_parameter_constants count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    multifile_test(
        "module consts\n  implicit none\n  integer, parameter :: MAX_N = 1024\n  integer, parameter :: HALF = MAX_N / 2\nend module\n",
        "program p\n  use consts\n  print *, MAX_N, HALF\nend program\n",
        "1024",
    );
}

#[test]
fn character_parameter_length_is_independent_of_unrelated_module_order() {
    for level in ["-O0", "-O2"] {
        if let Err(reason) = armfortas::testing::native_e2e_level_support(level) {
            eprintln!(
                "\nHARNESS_SKIP suite=multifile test=character_parameter_length_is_independent_of_unrelated_module_order count=4 reason=\"{}\"",
                reason
            );
            return;
        }
    }

    let compiler = find_compiler();
    let producer_orders = [
        (
            "foreign_first",
            "\
module foreign_m
  implicit none
  character(8), parameter :: seed = '12345678'
end module foreign_m

module victim_m
  implicit none
  character(1), parameter :: seed = 'Z'
  character(*), parameter :: copied = seed
end module victim_m
",
        ),
        (
            "victim_first",
            "\
module victim_m
  implicit none
  character(1), parameter :: seed = 'Z'
  character(*), parameter :: copied = seed
end module victim_m

module foreign_m
  implicit none
  character(8), parameter :: seed = '12345678'
end module foreign_m
",
        ),
    ];

    for (order, producer_source) in producer_orders {
        let dir = unique_dir();
        let producer_f90 = dir.join(format!("producer_{order}.f90"));
        let producer_o = dir.join(format!("producer_{order}.o"));
        let consumer_f90 = dir.join("consumer.f90");

        std::fs::write(&producer_f90, producer_source).unwrap();
        std::fs::write(
            &consumer_f90,
            "\
program consumer
  use victim_m, only: copied
  implicit none
  if (len(copied) /= 1) error stop 1
  if (iachar(copied(1:1)) /= iachar('Z')) error stop 2
  print *, len(copied), iachar(copied(1:1))
end program consumer
",
        )
        .unwrap();

        compile_file(&compiler, &producer_f90, &producer_o, None);
        let amod = std::fs::read_to_string(dir.join("victim_m.amod"))
            .expect("producer did not emit victim_m.amod");
        let copied = amod
            .lines()
            .find(|line| line.starts_with("@param copied :"))
            .expect("victim_m.amod omitted copied");
        assert!(
            copied.contains("character(len=1)"),
            "{order} serialized a non-lexical character length:\n{copied}"
        );

        for level in ["-O0", "-O2"] {
            let suffix = level.trim_start_matches('-');
            let consumer_o = dir.join(format!("consumer_{suffix}.o"));
            let binary = dir.join(format!("consumer_{suffix}"));
            compile_file_flags(&compiler, &consumer_f90, &consumer_o, Some(&dir), &[level]);
            link_files(&[&producer_o, &consumer_o], &binary);
            let output = run_binary(&binary);
            assert!(
                output_contains_expected(&output, "1 90"),
                "{order} consumer at {level} observed the wrong value:\n{output}"
            );
        }

        let _ = std::fs::remove_dir_all(&dir);
    }
}

#[test]
fn imported_logical_kinds_preserve_storage_and_semantics() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=imported_logical_kinds_preserve_storage_and_semantics count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    multifile_test(
        r#"module logical_kinds_m
  implicit none
  logical(1) :: narrow(3) = [.true._1, .false._1, .true._1]
  logical(8) :: wide(2) = [.false._8, .true._8]
  logical :: normal = .false.
  logical(1), parameter :: enabled = .true._1
end module
"#,
        r#"program p
  use logical_kinds_m, only: imported_narrow => narrow, imported_wide => wide, &
       imported_normal => normal, imported_enabled => enabled
  implicit none
  print '(7(l1,1x))', imported_narrow, imported_wide, imported_normal, imported_enabled
end program
"#,
        "T F T F T F T",
    );
}

#[test]
fn use_only_filtering() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=use_only_filtering count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    multifile_test(
        "module big_mod\n  implicit none\n  integer :: alpha = 10\n  integer :: beta = 20\n  integer :: gamma = 30\nend module\n",
        "program p\n  use big_mod, only: beta\n  print *, beta\nend program\n",
        "20",
    );
}

#[test]
fn use_only_excludes_defined_assignment_from_amod() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=use_only_excludes_defined_assignment_from_amod count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let t_f90 = dir.join("t.f90");
    let e_f90 = dir.join("e.f90");
    let main_f90 = dir.join("main.f90");
    let t_o = dir.join("t.o");
    let e_o = dir.join("e.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("test_bin");

    std::fs::write(
        &t_f90,
        r#"module t
  implicit none
  type :: v
    integer, allocatable :: a(:)
  end type
  interface assignment(=)
    module procedure asn
  end interface
contains
  function mk() result(r)
    type(v) :: r
    allocate(r%a(1))
    r%a(1) = 9
  end function

  subroutine asn(lhs, rhs)
    type(v), intent(out) :: lhs
    type(v), intent(in) :: rhs
    if (allocated(rhs%a)) print '(a)', 'defined assignment fired'
  end subroutine
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &e_f90,
        r#"module e
  use t, only: v, mk
  implicit none
contains
  function go() result(r)
    type(v) :: r
    r = mk()
  end function
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        r#"program p
  use t, only: v
  use e, only: go
  implicit none
  type(v) :: x
  x = go()
  print '(a,l1)', 'alloc=', allocated(x%a)
  if (.not. allocated(x%a)) stop 1
end program
"#,
    )
    .unwrap();

    compile_file(&compiler, &t_f90, &t_o, None);
    compile_file(&compiler, &e_f90, &e_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&t_o, &e_o, &main_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("alloc=T"),
        "expected intrinsic assignment to preserve allocatable component, got:\n{}",
        output
    );
    assert!(
        !output.contains("defined assignment fired"),
        "defined assignment leaked through USE ONLY:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn use_rename() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=use_rename count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    multifile_test(
        "module rename_mod\n  implicit none\n  integer :: original = 99\nend module\n",
        "program p\n  use rename_mod, renamed => original\n  print *, renamed\nend program\n",
        "99",
    );
}

#[test]
fn same_named_imported_types_keep_module_owned_layouts() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=same_named_imported_types_keep_module_owned_layouts count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let compiler = find_compiler();
    let dir = unique_dir();
    let alpha_f90 = dir.join("alpha_m.f90");
    let beta_f90 = dir.join("beta_m.f90");
    let main_f90 = dir.join("main.f90");
    let alpha_o = dir.join("alpha_m.o");
    let beta_o = dir.join("beta_m.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("test_bin");

    std::fs::write(
        &alpha_f90,
        "module alpha_m\n  implicit none\n  type :: item_t\n    integer :: value = 0\n  end type\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &beta_f90,
        "module beta_m\n  implicit none\n  type :: item_t\n    integer :: pad = -1\n    integer(8) :: value = 0\n  end type\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use alpha_m, only: alpha_item => item_t\n  use beta_m, only: beta_item => item_t\n  implicit none\n  type(alpha_item) :: alpha\n  type(beta_item) :: beta\n  alpha%value = 17\n  beta%pad = 23\n  beta%value = 5000000000_8\n  if (alpha%value /= 17) error stop 1\n  if (beta%pad /= 23) error stop 2\n  if (beta%value /= 5000000000_8) error stop 3\n  print *, 'ok'\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &alpha_f90, &alpha_o, None);
    compile_file(&compiler, &beta_f90, &beta_o, None);
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&alpha_o, &beta_o, &main_o], &binary);
    let output = run_binary(&binary);
    assert!(output.contains("ok"), "unexpected output:\n{}", output);

    let _ = std::fs::remove_dir_all(&dir);
}

/// Generic interface resolved across .amod boundaries: the consumer
/// reconstructs the NamedInterface from the @interface block and
/// dispatches each specific at the call site.
#[test]
fn generic_interface_cross_module() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=generic_interface_cross_module count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    multifile_test(
        "module mgen\n  implicit none\n  interface add\n    module procedure add_int, add_real\n  end interface\ncontains\n  integer function add_int(a, b)\n    integer, intent(in) :: a, b\n    add_int = a + b\n  end function\n  real function add_real(a, b)\n    real, intent(in) :: a, b\n    add_real = a + b\n  end function\nend module\n",
        "program p\n  use mgen\n  implicit none\n  integer :: integer_result\n  real :: real_result\n  integer_result = add(1, 2)\n  real_result = add(1.5, 2.5)\n  if (integer_result /= 3) error stop 1\n  if (abs(real_result - 4.0) > 1.0e-6) error stop 2\n  print '(a)', 'generic-dispatch-ok'\nend program\n",
        "generic-dispatch-ok",
    );
}

/// Generic interface reachable transitively through an intermediate
/// module that re-exports via `USE`. The middle module's .amod has
/// only `@uses base`; the consumer must recursively load base and
/// re-expose its symbols (including the NamedInterface) so generic
/// dispatch walks the chain.
#[test]
fn generic_interface_transitive_use() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=generic_interface_transitive_use count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let base_f90 = dir.join("base.f90");
    let middle_f90 = dir.join("middle.f90");
    let main_f90 = dir.join("main.f90");
    let base_o = dir.join("base.o");
    let middle_o = dir.join("middle.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("test_bin");

    std::fs::write(&base_f90, "module base\n  implicit none\n  interface add\n    module procedure add_int, add_real\n  end interface\ncontains\n  integer function add_int(a, b)\n    integer, intent(in) :: a, b\n    add_int = a + b\n  end function\n  real function add_real(a, b)\n    real, intent(in) :: a, b\n    add_real = a + b\n  end function\nend module\n").unwrap();
    std::fs::write(&middle_f90, "module middle\n  use base\nend module\n").unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use middle\n  print *, add(1, 2)\n  print *, add(1.5, 2.5)\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &base_f90, &base_o, None);
    compile_file(&compiler, &middle_f90, &middle_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &middle_o, &base_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("3"),
        "expected '3' in output, got:\n{}",
        output
    );
    assert!(
        output.contains("4.0000000E0"),
        "expected real add result in output, got:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn submodule_host_association_resolves_transitive_real_parameter() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=submodule_host_association_resolves_transitive_real_parameter count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let consts_f90 = dir.join("consts.f90");
    let middle_f90 = dir.join("middle.f90");
    let parent_f90 = dir.join("parent.f90");
    let body_f90 = dir.join("body.f90");
    let main_f90 = dir.join("main.f90");
    let consts_o = dir.join("consts.o");
    let middle_o = dir.join("middle.o");
    let parent_o = dir.join("parent.o");
    let body_o = dir.join("body.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("test_bin");

    std::fs::write(
        &consts_f90,
        "module consts_m\n  implicit none\n  public\n  real, parameter :: one = 1.0\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &middle_f90,
        "module middle_m\n  use consts_m\n  implicit none\n  public\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &parent_f90,
        "module parent_m\n  use middle_m\n  implicit none\n  private\n  interface\n    module subroutine fill(y)\n      real, intent(out) :: y\n    end subroutine\n  end interface\n  public :: fill\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &body_f90,
        "submodule(parent_m) parent_body\ncontains\n  module subroutine fill(y)\n    real, intent(out) :: y\n    y = one\n  end subroutine\nend submodule\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use parent_m, only: fill\n  implicit none\n  real :: y\n  call fill(y)\n  if (abs(y - 1.0) > 0.001) error stop 10\n  print *, 'ok'\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &consts_f90, &consts_o, None);
    compile_file(&compiler, &middle_f90, &middle_o, Some(&dir));
    compile_file(&compiler, &parent_f90, &parent_o, Some(&dir));
    compile_file(&compiler, &body_f90, &body_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(
        &[&main_o, &body_o, &parent_o, &middle_o, &consts_o],
        &binary,
    );
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "expected transitive parameter submodule body to print ok, got:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn generic_interface_beats_private_renamed_import() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=generic_interface_beats_private_renamed_import count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let dep_f90 = dir.join("dep.f90");
    let wrapper_f90 = dir.join("wrapper.f90");
    let main_f90 = dir.join("main.f90");
    let dep_o = dir.join("dep.o");
    let wrapper_o = dir.join("wrapper.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("test_bin");

    std::fs::write(
        &dep_f90,
        "module dep\n  implicit none\ncontains\n  integer function pick(x)\n    integer, intent(in) :: x\n    pick = -1\n  end function\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &wrapper_f90,
        "module wrapper\n  use dep, only: pick_dep => pick\n  implicit none\n  private\n  public :: box, pick\n  type :: box\n    integer :: v\n  end type\n  interface pick\n    module procedure pick_box\n  end interface\ncontains\n  integer function pick_box(x)\n    type(box), intent(in) :: x\n    pick_box = x%v\n  end function\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use wrapper, only: box, pick\n  implicit none\n  type(box) :: b\n  b%v = 42\n  print *, pick(b)\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &dep_f90, &dep_o, None);
    compile_file(&compiler, &wrapper_f90, &wrapper_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &wrapper_o, &dep_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("42"),
        "expected wrapper generic to dispatch to pick_box, got:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_deferred_character_results_preserve_ownership() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=imported_deferred_character_results_preserve_ownership count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("character_results.f90");
    let main_f90 = dir.join("main.f90");
    let mod_o = dir.join("character_results.o");
    let main_o = dir.join("main.o");
    let main_ir = dir.join("main.ir");
    let binary = dir.join("test_bin");

    std::fs::write(
        &mod_f90,
        r#"module imported_character_results_m
  implicit none

  interface make_generic
    module procedure make_owned_generic, make_borrowed_generic
  end interface

  type :: producer_t
  contains
    procedure :: make_owned_bound
    procedure :: make_borrowed_bound
  end type

contains
  function make_owned() result(value)
    character(:), allocatable :: value
    value = 'owned'
  end function

  function make_borrowed() result(value)
    character(:), pointer :: value
    character(8), target, save :: storage = 'borrowed'
    value => storage
  end function

  function make_owned_generic(selector) result(value)
    integer, intent(in) :: selector
    character(:), allocatable :: value
    value = 'generic owned'
  end function

  function make_borrowed_generic(selector) result(value)
    logical, intent(in) :: selector
    character(:), pointer :: value
    character(16), target, save :: storage = 'generic borrowed'
    value => storage
  end function

  function make_owned_bound(self) result(value)
    class(producer_t), intent(in) :: self
    character(:), allocatable :: value
    value = 'bound owned'
  end function

  function make_borrowed_bound(self) result(value)
    class(producer_t), intent(in) :: self
    character(:), pointer :: value
    character(14), target, save :: storage = 'bound borrowed'
    value => storage
  end function
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        r#"program p
  use imported_character_results_m, only: imported_owned => make_owned, &
       imported_borrowed => make_borrowed, make_generic, producer_t
  implicit none
  character(16) :: sink
  type(producer_t) :: producer

  sink = imported_owned()
  if (sink(1:5) /= 'owned') error stop 1
  sink = imported_borrowed()
  if (sink(1:8) /= 'borrowed') error stop 2
  sink = make_generic(1)
  if (sink(1:13) /= 'generic owned') error stop 3
  sink = make_generic(.true.)
  if (sink /= 'generic borrowed') error stop 4
  sink = producer%make_owned_bound()
  if (sink(1:11) /= 'bound owned') error stop 5
  sink = producer%make_borrowed_bound()
  if (sink(1:14) /= 'bound borrowed') error stop 6
  print *, 'ok'
end program
"#,
    )
    .unwrap();

    compile_file(&compiler, &mod_f90, &mod_o, None);
    let amod = std::fs::read_to_string(dir.join("imported_character_results_m.amod")).unwrap();
    assert!(
        amod.contains("@function make_owned -> character")
            && amod.contains("result_allocatable")
            && amod.contains("@function make_borrowed -> character")
            && amod.contains("result_pointer"),
        "deferred-character result ownership missing from module interface:\n{}",
        amod
    );

    let emit = Command::new(&compiler)
        .current_dir(&dir)
        .arg("--emit-ir")
        .arg(&main_f90)
        .arg(format!("-I{}", dir.display()))
        .arg("-o")
        .arg(&main_ir)
        .output()
        .expect("consumer IR emission failed to spawn");
    assert!(
        emit.status.success(),
        "consumer IR emission failed: {}",
        String::from_utf8_lossy(&emit.stderr)
    );
    let ir = std::fs::read_to_string(&main_ir).expect("cannot read consumer IR");
    let program_start = ir
        .find("func @__prog_p")
        .expect("missing consumer program IR");
    let program_tail = &ir[program_start..];
    let program_end = program_tail
        .find("\n  func @")
        .unwrap_or(program_tail.len());
    let program_ir = &program_tail[..program_end];
    let call_markers = [
        ("make_owned", true),
        ("make_borrowed", false),
        ("make_owned_generic", true),
        ("make_borrowed_generic", false),
        ("make_owned_bound", true),
        ("make_borrowed_bound", false),
    ];
    let call_offsets: Vec<_> = call_markers
        .iter()
        .map(|(name, owned)| {
            let marker = format!("call @afs_modproc_imported_character_results_m_{}", name);
            (
                program_ir
                    .find(&marker)
                    .unwrap_or_else(|| panic!("missing imported result call {marker}")),
                *name,
                *owned,
            )
        })
        .collect();
    for (index, &(start, name, owned)) in call_offsets.iter().enumerate() {
        let end = call_offsets
            .get(index + 1)
            .map(|entry| entry.0)
            .unwrap_or(program_ir.len());
        let segment = &program_ir[start..end];
        assert_eq!(
            segment.contains("rt_call @__afs_deallocate"),
            owned,
            "imported result ownership was lowered incorrectly for {name}:\n{segment}"
        );
    }
    assert_eq!(
        program_ir.matches("rt_call @__afs_deallocate").count(),
        3,
        "only imported allocatable character results should be released:\n{}",
        program_ir
    );

    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &mod_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "imported deferred-character calls returned wrong values:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_type_bound_result_guides_operator_generic() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=imported_type_bound_result_guides_operator_generic count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let string_f90 = dir.join("string_mod.f90");
    let list_f90 = dir.join("list_mod.f90");
    let main_f90 = dir.join("main.f90");
    let string_o = dir.join("string_mod.o");
    let list_o = dir.join("list_mod.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("test_bin");

    std::fs::write(
        &string_f90,
        r#"module string_mod
  implicit none
  private
  public :: string_type, operator(==)

  type :: string_type
    character(len=:), allocatable :: raw
  end type

  interface string_type
    module procedure new_string
  end interface

  interface operator(==)
    module procedure eq_char_string
    module procedure eq_string_char
    module procedure eq_string_string
  end interface

contains
  function new_string(raw) result(s)
    character(len=*), intent(in) :: raw
    type(string_type) :: s
    s%raw = raw
  end function

  logical function eq_string_string(lhs, rhs)
    type(string_type), intent(in) :: lhs
    type(string_type), intent(in) :: rhs
    eq_string_string = allocated(lhs%raw) .eqv. allocated(rhs%raw)
    if (eq_string_string .and. allocated(lhs%raw)) eq_string_string = lhs%raw == rhs%raw
  end function

  logical function eq_string_char(lhs, rhs)
    type(string_type), intent(in) :: lhs
    character(len=*), intent(in) :: rhs
    eq_string_char = allocated(lhs%raw)
    if (eq_string_char) eq_string_char = lhs%raw == rhs
  end function

  logical function eq_char_string(lhs, rhs)
    character(len=*), intent(in) :: lhs
    type(string_type), intent(in) :: rhs
    eq_char_string = allocated(rhs%raw)
    if (eq_char_string) eq_char_string = lhs == rhs%raw
  end function
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &list_f90,
        r#"module list_mod
  use string_mod, only: string_type
  implicit none
  private
  public :: list_type

  type :: list_type
    type(string_type) :: value
  contains
    procedure :: get
  end type

contains
  function get(list) result(value)
    class(list_type), intent(in) :: list
    type(string_type) :: value
    value = list%value
  end function
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        r#"program p
  use string_mod, only: string_type, operator(==)
  use list_mod, only: list_type
  implicit none
  type(list_type) :: list

  list%value = string_type("ok")
  if (.not. (list%get() == string_type("ok"))) error stop 1
  print *, "ok"
end program
"#,
    )
    .unwrap();

    compile_file(&compiler, &string_f90, &string_o, None);
    compile_file(&compiler, &list_f90, &list_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &list_o, &string_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "expected imported TBP result to dispatch eq_string_string, got:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn module_private_default() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=module_private_default count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    // F2008 §12.2.3.2: submodules of a module see *all* parent entities,
    // including the privates. The .amod must therefore round-trip private
    // module variables — but tagged `private` so module-level USE
    // associations reject them while submodule host association accepts.
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("mod.f90");
    let mod_o = dir.join("mod.o");

    std::fs::write(&mod_f90,
        "module priv_mod\n  implicit none\n  private\n  integer, public :: pub_val = 42\n  integer :: priv_val = 99\nend module\n"
    ).unwrap();
    compile_file(&compiler, &mod_f90, &mod_o, None);

    let amod = std::fs::read_to_string(dir.join("priv_mod.amod")).unwrap();
    let pub_line = amod
        .lines()
        .find(|l| l.contains("pub_val"))
        .expect("pub_val should appear in .amod");
    assert!(
        !pub_line.contains("private"),
        "pub_val should not carry the `private` annotation: {pub_line}"
    );
    let priv_line = amod
        .lines()
        .find(|l| l.contains("priv_val"))
        .expect("priv_val should appear in .amod (with `private` annotation) so submodule host association can resolve it");
    assert!(
        priv_line.contains("private"),
        "priv_val must be tagged `private` in the .amod: {priv_line}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// AR40-05: private derived-type layouts remain serialized for descendant
// submodules, but their symbols must retain PRIVATE accessibility when an
// ordinary consumer reconstructs the provider from its .amod.
#[test]
fn private_derived_type_access_round_trips_through_amod() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=private_derived_type_access_round_trips_through_amod count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let compiler = find_compiler();
    let dir = unique_dir();
    for (case, module_name, declarations) in [
        (
            "default_private",
            "default_private_types",
            "  private\n  public :: visible_t\n\n  type :: hidden_t\n    integer :: value\n  end type hidden_t\n\n  type :: visible_t\n    integer :: value\n  end type visible_t\n",
        ),
        (
            "explicit_private",
            "explicit_private_types",
            "  type, private :: hidden_t\n    integer :: value\n  end type hidden_t\n\n  type :: visible_t\n    integer :: value\n  end type visible_t\n",
        ),
    ] {
        let case_dir = dir.join(case);
        std::fs::create_dir_all(&case_dir).unwrap();
        let provider_f90 = case_dir.join("provider.f90");
        let provider_o = case_dir.join("provider.o");
        let hidden_f90 = case_dir.join("hidden_consumer.f90");
        let hidden_o = case_dir.join("hidden_consumer.o");
        let bare_hidden_f90 = case_dir.join("bare_hidden_consumer.f90");
        let bare_hidden_o = case_dir.join("bare_hidden_consumer.o");
        let child_f90 = case_dir.join("child.f90");
        let child_o = case_dir.join("child.o");
        let visible_f90 = case_dir.join("visible_consumer.f90");
        let visible_o = case_dir.join("visible_consumer.o");

        std::fs::write(
            &provider_f90,
            format!(
                "module {module_name}\n  implicit none\n{declarations}end module {module_name}\n"
            ),
        )
        .unwrap();
        std::fs::write(
            &hidden_f90,
            format!(
                "program hidden_consumer\n  use {module_name}, only: hidden_t\n  implicit none\n  type(hidden_t) :: item\n  item%value = 17\nend program hidden_consumer\n"
            ),
        )
        .unwrap();
        std::fs::write(
            &bare_hidden_f90,
            format!(
                "program bare_hidden_consumer\n  use {module_name}\n  implicit none\n  type(hidden_t) :: item\n  item%value = 19\nend program bare_hidden_consumer\n"
            ),
        )
        .unwrap();
        std::fs::write(
            &child_f90,
            format!(
                "submodule({module_name}) child\n  implicit none\n  type(hidden_t) :: item\nend submodule child\n"
            ),
        )
        .unwrap();
        std::fs::write(
            &visible_f90,
            format!(
                "program visible_consumer\n  use {module_name}, only: visible_t\n  implicit none\n  type(visible_t) :: item\n  item%value = 23\nend program visible_consumer\n"
            ),
        )
        .unwrap();

        compile_file(&compiler, &provider_f90, &provider_o, None);

        let rejected = Command::new(&compiler)
            .current_dir(&case_dir)
            .args([
                hidden_f90.to_str().unwrap(),
                "-c",
                "-o",
                hidden_o.to_str().unwrap(),
            ])
            .arg(format!("-I{}", case_dir.display()))
            .output()
            .expect("private-type consumer compiler launch failed");
        assert!(
            !rejected.status.success(),
            "{case}: a private derived type reconstructed from .amod was USE-associated"
        );
        let stderr = String::from_utf8_lossy(&rejected.stderr);
        assert!(
            stderr.contains(&format!(
                "USE target 'hidden_t' is not exported by module '{module_name}'"
            )),
            "{case}: source and .amod private-type diagnostics diverged:\n{stderr}"
        );
        assert!(
            !hidden_o.exists(),
            "{case}: rejected private-type consumer left an object file"
        );

        let bare_rejected = Command::new(&compiler)
            .current_dir(&case_dir)
            .args([
                bare_hidden_f90.to_str().unwrap(),
                "-c",
                "-o",
                bare_hidden_o.to_str().unwrap(),
            ])
            .arg(format!("-I{}", case_dir.display()))
            .output()
            .expect("bare-USE private-type consumer compiler launch failed");
        assert!(
            !bare_rejected.status.success(),
            "{case}: bare USE exposed a private derived type reconstructed from .amod"
        );
        let bare_stderr = String::from_utf8_lossy(&bare_rejected.stderr);
        assert!(
            bare_stderr.contains("derived type 'hidden_t' is not accessible in this scope"),
            "{case}: bare-USE private-type diagnostic was not explicit:\n{bare_stderr}"
        );
        assert!(
            !bare_hidden_o.exists(),
            "{case}: rejected bare-USE private-type consumer left an object file"
        );

        compile_file(&compiler, &visible_f90, &visible_o, Some(&case_dir));
        compile_file(&compiler, &child_f90, &child_o, Some(&case_dir));

        let amod = std::fs::read_to_string(case_dir.join(format!("{module_name}.amod"))).unwrap();
        let hidden_line = amod
            .lines()
            .find(|line| line.starts_with("@type hidden_t"))
            .expect("private layout must remain in .amod for submodule host association");
        let visible_line = amod
            .lines()
            .find(|line| line.starts_with("@type visible_t"))
            .expect("public layout must be present in .amod");
        assert!(
            hidden_line.ends_with(", private"),
            "{case}: private type accessibility missing from .amod: {hidden_line}"
        );
        assert!(
            visible_line.ends_with(", public"),
            "{case}: public type accessibility missing from .amod: {visible_line}"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

// AR40-06: component accessibility is part of a derived type's semantic
// interface. It must survive .amod reconstruction without hiding the layout
// bytes required by descendant submodules and extending types.
#[test]
fn private_component_access_round_trips_through_amod() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=private_component_access_round_trips_through_amod count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let compiler = find_compiler();
    let dir = unique_dir();
    let provider_f90 = dir.join("provider.f90");
    let provider_o = dir.join("provider.o");
    let consumer_f90 = dir.join("consumer.f90");
    let consumer_o = dir.join("consumer.o");
    let public_consumer_f90 = dir.join("public_consumer.f90");
    let public_consumer_o = dir.join("public_consumer.o");
    let child_f90 = dir.join("child.f90");
    let child_o = dir.join("child.o");

    std::fs::write(
        &provider_f90,
        "\
module component_provider
  implicit none
  private
  public :: explicit_box, default_box, extended_box, set_hidden

  abstract interface
    subroutine callback_iface()
    end subroutine callback_iface
  end interface

  type :: explicit_box
    integer, private :: explicit_hidden = 0
    integer, public :: explicit_shown = 0
    procedure(callback_iface), pointer, private, nopass :: private_callback
  end type explicit_box

  type :: default_box
    private
    integer :: default_hidden = 0
    integer, public :: default_shown = 0
  end type default_box

  type, extends(default_box) :: extended_box
    integer, public :: extended_shown = 0
  end type extended_box

  interface
    module subroutine set_hidden(value)
      type(default_box), intent(inout) :: value
    end subroutine set_hidden
  end interface
end module component_provider
",
    )
    .unwrap();
    std::fs::write(
        &consumer_f90,
        "\
module foreign_extension
  use component_provider, only: default_box
  implicit none
  type, extends(default_box) :: child_box
    integer :: child_value = 0
  end type child_box
contains
  subroutine touch(value)
    type(child_box), intent(inout) :: value
    value%default_hidden = 1
  end subroutine touch
end module foreign_extension

program private_consumer
  use component_provider, only: renamed_box => explicit_box, default_box
  implicit none
  type(renamed_box) :: left
  type(default_box) :: right
  left%explicit_hidden = 2
  right%default_hidden = 3
  if (associated(left%private_callback)) stop 1
  left = renamed_box(explicit_hidden=4)
  right = default_box(5)
  associate (alias => left)
    alias%explicit_hidden = 6
  end associate
end program private_consumer
",
    )
    .unwrap();
    std::fs::write(
        &public_consumer_f90,
        "\
program public_consumer
  use component_provider, only: explicit_box, default_box, extended_box
  implicit none
  type(explicit_box) :: left
  type(default_box) :: right
  type(extended_box) :: extended
  left%explicit_shown = 7
  right%default_shown = 8
  left = explicit_box(explicit_shown=9)
  right = default_box(default_shown=10)
  extended = extended_box(default_box(default_shown=11), 12)
  extended = extended_box(default_box=default_box(default_shown=13), extended_shown=14)
end program public_consumer
",
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        "\
submodule(component_provider) component_child
contains
  module subroutine set_hidden(value)
    type(default_box), intent(inout) :: value
    value%default_hidden = 11
  end subroutine set_hidden
end submodule component_child
",
    )
    .unwrap();

    compile_file(&compiler, &provider_f90, &provider_o, None);

    let amod = std::fs::read_to_string(dir.join("component_provider.amod")).expect("missing .amod");
    for (field, access) in [
        ("explicit_hidden", "private"),
        ("explicit_shown", "public"),
        ("private_callback", "private"),
        ("default_hidden", "private"),
        ("default_shown", "public"),
    ] {
        let line = amod
            .lines()
            .find(|line| line.trim_start().starts_with(&format!("@field {field} ")))
            .unwrap_or_else(|| panic!("missing {field} field record:\n{amod}"));
        assert!(
            line.contains(&format!("@access {access}"))
                && line.contains("@owner component_provider"),
            "{field} accessibility/owner missing from .amod: {line}"
        );
    }

    let rejected = Command::new(&compiler)
        .current_dir(&dir)
        .args([
            consumer_f90.to_str().unwrap(),
            "-c",
            "-o",
            consumer_o.to_str().unwrap(),
        ])
        .arg(format!("-I{}", dir.display()))
        .output()
        .expect("private-component consumer compiler launch failed");
    assert!(
        !rejected.status.success(),
        "ordinary and foreign-extending consumers accessed private components"
    );
    let stderr = String::from_utf8_lossy(&rejected.stderr);
    assert_eq!(
        stderr.matches("private component").count(),
        7,
        "private-component diagnostics were incomplete:\n{stderr}"
    );
    assert!(
        !consumer_o.exists(),
        "rejected private-component consumer left an object file"
    );

    compile_file(
        &compiler,
        &public_consumer_f90,
        &public_consumer_o,
        Some(&dir),
    );
    compile_file(&compiler, &child_f90, &child_o, Some(&dir));

    let _ = std::fs::remove_dir_all(&dir);
}

// AR40-01: a separate module function's RESULT identity is declared by its
// interface. Unrelated locals and named constants must not affect same-TU
// lowering or the identity serialized for cross-TU compilation.
#[test]
fn separate_module_function_result_identity_ignores_unrelated_symbols() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=separate_module_function_result_identity_ignores_unrelated_symbols count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let parent_f90 = dir.join("result_parent.f90");
    let child_f90 = dir.join("result_child.f90");
    let main_f90 = dir.join("result_main.f90");
    let parent_o = dir.join("result_parent.o");
    let child_o = dir.join("result_child.o");
    let main_o = dir.join("result_main.o");
    let single_f90 = dir.join("result_single.f90");
    let single_bin = dir.join("result_single_bin");
    let cross_tu_bin = dir.join("result_cross_tu_bin");

    let parent_source = r#"module result_parent
  implicit none
  interface
    module function answer() result(actual_result)
      integer, parameter :: decoy_a = 101
      integer, parameter :: decoy_b = 102
      integer, parameter :: decoy_c = 103
      integer, parameter :: decoy_d = 104
      integer, parameter :: decoy_e = 105
      integer, parameter :: decoy_f = 106
      integer, parameter :: decoy_g = 107
      integer, parameter :: decoy_h = 108
      integer, parameter :: decoy_i = 109
      integer, parameter :: decoy_j = 110
      integer, parameter :: decoy_k = 111
      integer, parameter :: decoy_l = 112
      integer, parameter :: decoy_m = 113
      integer, parameter :: decoy_n = 114
      integer, parameter :: decoy_o = 115
      integer, parameter :: decoy_p = 116
      integer :: actual_result
    end function answer
  end interface
end module result_parent
"#;
    let child_source = r#"submodule (result_parent) result_child
contains
  module procedure answer
    actual_result = 42
  end procedure answer
end submodule result_child
"#;
    let main_source = r#"program result_main
  use result_parent, only : answer
  implicit none
  if (answer() /= 42) error stop 1
  print '(a)', 'ok'
end program result_main
"#;

    std::fs::write(&parent_f90, parent_source).unwrap();
    std::fs::write(&child_f90, child_source).unwrap();
    std::fs::write(&main_f90, main_source).unwrap();
    std::fs::write(
        &single_f90,
        format!("{parent_source}\n{child_source}\n{main_source}"),
    )
    .unwrap();

    for attempt in 0..8 {
        let single_compile = Command::new(&compiler)
            .current_dir(&dir)
            .args([
                single_f90.to_str().unwrap(),
                "-o",
                single_bin.to_str().unwrap(),
            ])
            .output()
            .expect("single-TU result-identity compile failed to spawn");
        assert!(
            single_compile.status.success(),
            "single-TU result-identity compile attempt {attempt} failed:\n{}",
            String::from_utf8_lossy(&single_compile.stderr)
        );
        let single_run = Command::new(&single_bin)
            .output()
            .expect("single-TU result-identity binary failed to spawn");
        assert!(
            single_run.status.success()
                && String::from_utf8_lossy(&single_run.stdout).contains("ok"),
            "same-TU compile attempt {attempt} selected an unrelated symbol as its result:\nstatus={:?}\nstdout={}\nstderr={}",
            single_run.status.code(),
            String::from_utf8_lossy(&single_run.stdout),
            String::from_utf8_lossy(&single_run.stderr)
        );
    }

    compile_file(&compiler, &parent_f90, &parent_o, None);
    let amod = std::fs::read_to_string(dir.join("result_parent.amod")).unwrap();
    let function_record = amod
        .lines()
        .find(|line| line.starts_with("@function answer "))
        .expect("answer function record missing from result_parent.amod");
    assert!(
        function_record.contains("result_name=actual_result"),
        "serialized function lost its explicit result identity: {function_record}"
    );
    compile_file(&compiler, &child_f90, &child_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &child_o, &parent_o], &cross_tu_bin);
    let cross_tu_output = run_binary(&cross_tu_bin);
    assert!(
        cross_tu_output.contains("ok"),
        "cross-TU separate module function lost its serialized result identity:\n{cross_tu_output}"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// l07: a separately-compiled submodule whose body implements a parent
// MODULE FUNCTION must return the right type. The result variable's type
// comes from the parent interface via the `.amod`; before l07 it fell to
// implicit typing (an integer result named `r` became REAL, returned in a
// different register than the caller read) — a silent wrong answer. Covers
// both the with-args and no-arg function forms plus a subroutine control.
#[test]
fn cross_tu_submodule_scalar_function_returns_correct_type() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_submodule_scalar_function_returns_correct_type count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let parent_f90 = dir.join("sm_parent.f90");
    let child_f90 = dir.join("sm_child.f90");
    let main_f90 = dir.join("sm_main.f90");
    let parent_o = dir.join("sm_parent.o");
    let child_o = dir.join("sm_child.o");
    let main_o = dir.join("sm_main.o");
    let binary = dir.join("sm_bin");

    std::fs::write(
        &parent_f90,
        r#"module sm
  implicit none
  interface
    module function dbl(x) result(r)
      integer, intent(in) :: x
      integer :: r
    end function
    module function answer() result(r)
      integer :: r
    end function
    module subroutine setit(v)
      integer, intent(out) :: v
    end subroutine
  end interface
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        r#"submodule (sm) sm_impl
contains
  module procedure dbl
    r = 2 * x
  end procedure
  module procedure answer
    r = 42
  end procedure
  module procedure setit
    v = 99
  end procedure
end submodule
"#,
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        r#"program p
  use sm
  implicit none
  integer :: v
  if (dbl(21) /= 42) error stop 1
  if (answer() /= 42) error stop 2
  call setit(v)
  if (v /= 99) error stop 3
  print *, "ok"
end program
"#,
    )
    .unwrap();

    compile_file(&compiler, &parent_f90, &parent_o, None);
    compile_file(&compiler, &child_f90, &child_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &child_o, &parent_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "cross-TU submodule scalar function returned wrong value (or wrong register):\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_submodule_array_function_passes_explicit_shape_actuals_by_data_pointer() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_submodule_array_function_passes_explicit_shape_actuals_by_data_pointer count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let parent_f90 = dir.join("cross_parent.f90");
    let child_f90 = dir.join("cross_child.f90");
    let main_f90 = dir.join("cross_main.f90");
    let parent_o = dir.join("cross_parent.o");
    let child_o = dir.join("cross_child.o");
    let main_o = dir.join("cross_main.o");
    let binary = dir.join("cross_bin");

    std::fs::write(
        &parent_f90,
        r#"module cross_mod
  implicit none
  interface
    module function cross_i(a, b) result(res)
      integer, intent(in) :: a(3), b(3)
      integer :: res(3)
    end function
  end interface
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        r#"submodule (cross_mod) cross_impl
contains
  pure module function cross_i(a, b) result(res)
    integer, intent(in) :: a(3), b(3)
    integer :: res(3)
    res(1) = a(2) * b(3) - a(3) * b(2)
    res(2) = a(3) * b(1) - a(1) * b(3)
    res(3) = a(1) * b(2) - a(2) * b(1)
  end function
end submodule
"#,
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        r#"program p
  use cross_mod, only: cross_i
  implicit none
  integer :: u(3), v(3), expected(3), diff(3)

  u = [1, 0, 0]
  v = [0, 1, 0]
  expected = [0, 0, 1]
  diff = expected - cross_i(u, v)
  if (any(diff /= 0)) error stop 1
  print *, "ok"
end program
"#,
    )
    .unwrap();

    compile_file(&compiler, &parent_f90, &parent_o, None);
    compile_file(&compiler, &child_f90, &child_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &child_o, &parent_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "cross-TU submodule array result returned wrong values:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn imported_interface_preserves_procedure_pointer_array_result_shape() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=imported_interface_preserves_procedure_pointer_array_result_shape count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let provider_f90 = dir.join("array_factory_provider.f90");
    let consumer_f90 = dir.join("array_factory_consumer.f90");
    let provider_o = dir.join("array_factory_provider.o");
    let consumer_o = dir.join("array_factory_consumer.o");
    let binary = dir.join("array_factory_bin");

    std::fs::write(
        &provider_f90,
        r#"module array_factory_provider
  implicit none
  abstract interface
    function array_factory(n) result(values)
      integer, intent(in) :: n
      integer :: values(2, n)
    end function array_factory
  end interface
contains
  function build_values(n) result(values)
    integer, intent(in) :: n
    integer :: values(2, n)
    values(1, 1) = 11
    values(2, 1) = 12
    values(1, 2) = 21
    values(2, 2) = 22
    values(1, 3) = 31
    values(2, 3) = 32
  end function build_values
end module array_factory_provider
"#,
    )
    .unwrap();
    std::fs::write(
        &consumer_f90,
        r#"program p
  use array_factory_provider, only: array_factory, build_values
  implicit none
  procedure(array_factory), pointer :: make_values
  integer :: got(2, 3)

  make_values => build_values
  got = make_values(3)
  if (any(got /= reshape([11, 12, 21, 22, 31, 32], [2, 3]))) error stop 1
  print *, "ok"
end program p
"#,
    )
    .unwrap();

    compile_file(&compiler, &provider_f90, &provider_o, None);
    let amod = std::fs::read_to_string(dir.join("array_factory_provider.amod"))
        .expect("missing provider module artifact");
    assert!(
        amod.contains(
            "@function array_factory -> integer, result_rank=2, result_name=values, result_array_bounds=\"(2; n)\""
        ),
        "abstract interface array-result metadata missing from .amod:\n{}",
        amod
    );
    compile_file(&compiler, &consumer_f90, &consumer_o, Some(&dir));
    link_files(&[&provider_o, &consumer_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "imported procedure-pointer array result returned wrong values:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_submodule_allocatable_array_result_preserves_amod_abi() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_submodule_allocatable_array_result_preserves_amod_abi count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let parent_f90 = dir.join("alloc_parent.f90");
    let child_f90 = dir.join("alloc_child.f90");
    let main_f90 = dir.join("alloc_main.f90");
    let parent_o = dir.join("alloc_parent.o");
    let child_o = dir.join("alloc_child.o");
    let main_o = dir.join("alloc_main.o");
    let binary = dir.join("alloc_bin");

    std::fs::write(
        &parent_f90,
        r#"module alloc_parent
  implicit none
  interface
    module function make_square(n) result(a)
      integer, intent(in) :: n
      real, allocatable :: a(:, :)
    end function
  end interface
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        r#"submodule (alloc_parent) alloc_impl
contains
  module function make_square(n) result(a)
    integer, intent(in) :: n
    real, allocatable :: a(:, :)
    integer :: i
    allocate(a(n, n))
    a = 0.0
    do i = 1, n
      a(i, i) = real(i)
    end do
  end function
end submodule
"#,
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        r#"program p
  use alloc_parent, only: make_square
  implicit none
  real, allocatable :: a(:, :)

  a = make_square(3)
  if (.not. allocated(a)) error stop 1
  if (size(a, 1) /= 3 .or. size(a, 2) /= 3) error stop 2
  if (abs(a(1, 1) - 1.0) > 1.0e-6) error stop 3
  if (abs(a(2, 2) - 2.0) > 1.0e-6) error stop 4
  if (abs(a(3, 3) - 3.0) > 1.0e-6) error stop 5
  print *, "ok"
end program
"#,
    )
    .unwrap();

    compile_file(&compiler, &parent_f90, &parent_o, None);
    let amod = std::fs::read_to_string(dir.join("alloc_parent.amod")).unwrap();
    assert!(
        amod.contains("@function make_square -> real, result_allocatable, result_rank=2"),
        "allocatable module-function result ABI missing from parent .amod:\n{}",
        amod
    );
    compile_file(&compiler, &child_f90, &child_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &child_o, &parent_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "cross-TU submodule allocatable array result returned wrong values:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_submodule_scalar_function_call_broadcasts_to_descriptor_array() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_submodule_scalar_function_call_broadcasts_to_descriptor_array count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let parent_f90 = dir.join("broadcast_parent.f90");
    let child_f90 = dir.join("broadcast_child.f90");
    let main_f90 = dir.join("broadcast_main.f90");
    let parent_o = dir.join("broadcast_parent.o");
    let child_o = dir.join("broadcast_child.o");
    let main_o = dir.join("broadcast_main.o");
    let binary = dir.join("broadcast_bin");

    std::fs::write(
        &parent_f90,
        r#"module broadcast_parent
  implicit none
  interface
    module function wrap(a, order) result(e)
      real, intent(in) :: a(:, :)
      integer, optional, intent(in) :: order
      real, allocatable :: e(:, :)
    end function
    module subroutine mark_inplace(a, order)
      real, intent(inout) :: a(:, :)
      integer, optional, intent(in) :: order
    end subroutine
  end interface
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        r#"submodule (broadcast_parent) broadcast_impl
contains
  module function wrap(a, order) result(e)
    real, intent(in) :: a(:, :)
    integer, optional, intent(in) :: order
    real, allocatable :: e(:, :)
    e = a
    call mark_inplace(e, order)
  end function

  module subroutine mark_inplace(a, order)
    real, intent(inout) :: a(:, :)
    integer, optional, intent(in) :: order
    if (present(order)) then
      a = real(order)
    else
      a = 11.0
    end if
  end subroutine
end submodule
"#,
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        r#"program p
  use broadcast_parent, only: wrap
  implicit none
  real :: a(2, 2)
  real, allocatable :: e(:, :)

  a = 1.0
  e = wrap(a)
  if (any(abs(e - 11.0) > 1.0e-6)) error stop 1
  e = wrap(a, order=3)
  if (any(abs(e - 3.0) > 1.0e-6)) error stop 2
  print *, "ok"
end program
"#,
    )
    .unwrap();

    compile_file(&compiler, &parent_f90, &parent_o, None);
    compile_file(&compiler, &child_f90, &child_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &child_o, &parent_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "cross-TU submodule scalar function-call broadcast failed:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn submodule_runtime_shape_local_uses_dummy_size_not_global_shadow() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=submodule_runtime_shape_local_uses_dummy_size_not_global_shadow count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let parent_f90 = dir.join("shape_parent.f90");
    let child_f90 = dir.join("shape_child.f90");
    let main_f90 = dir.join("shape_main.f90");
    let parent_o = dir.join("shape_parent.o");
    let child_o = dir.join("shape_child.o");
    let main_o = dir.join("shape_main.o");
    let binary = dir.join("shape_bin");

    std::fs::write(
        &parent_f90,
        r#"module shape_parent
  implicit none
  real :: a(1, 1)
  interface
    module subroutine fill(a)
      real, intent(inout) :: a(:, :)
    end subroutine
  end interface
end module
"#,
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        r#"submodule (shape_parent) shape_impl
contains
  module subroutine fill(a)
    real, intent(inout) :: a(:, :)
    real :: tmp(size(a, 1), size(a, 2))
    integer :: i, j

    do j = 1, size(a, 2)
      do i = 1, size(a, 1)
        tmp(i, j) = 10.0 * real(i) + real(j)
      end do
    end do
    a = tmp
  end subroutine
end submodule
"#,
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        r#"program p
  use shape_parent, only: fill
  implicit none
  real :: x(5, 5)

  x = 0.0
  call fill(x)
  if (abs(x(5, 5) - 55.0) > 1.0e-6) error stop 1
  print *, "ok"
end program
"#,
    )
    .unwrap();

    compile_file(&compiler, &parent_f90, &parent_o, None);
    compile_file(&compiler, &child_f90, &child_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&main_o, &child_o, &parent_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "submodule runtime-shape local used the wrong size() binding:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// l07 DoD: the multi-source driver (`armfortas a.f90 b.f90 ...` in one
// invocation) topologically orders submodules after their parents, even
// when files are given in the worst order. Before l07's dep_scan support,
// the submodule compiled before its parent's `.amod` existed and produced
// a silent wrong answer.
#[test]
fn multi_source_submodule_wrong_order_builds_and_runs() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=multi_source_submodule_wrong_order_builds_and_runs count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let parent_f90 = dir.join("ms_parent.f90");
    let child_f90 = dir.join("ms_child.f90");
    let main_f90 = dir.join("ms_main.f90");
    let binary = dir.join("ms_bin");

    std::fs::write(
        &parent_f90,
        "module ms\n  implicit none\n  interface\n    module function dbl(x) result(r)\n      integer, intent(in) :: x\n      integer :: r\n    end function\n  end interface\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        "submodule (ms) ms_impl\ncontains\n  module procedure dbl\n    r = 2 * x\n  end procedure\nend submodule\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use ms\n  if (dbl(21) /= 42) error stop 1\n  print *, \"ok\"\nend program\n",
    )
    .unwrap();

    // Deliberately worst order: child before parent.
    let result = std::process::Command::new(&compiler)
        .current_dir(&dir)
        .args([
            child_f90.to_str().unwrap(),
            parent_f90.to_str().unwrap(),
            main_f90.to_str().unwrap(),
            "-o",
            binary.to_str().unwrap(),
        ])
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "multi-source submodule build failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "multi-source submodule wrong-order run gave wrong answer:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// l07: a type-bound procedure whose target is a separate module procedure,
// with the module and its submodule in separate TUs. Exercises the
// TBP-thunk ownership rule across compilation units (the thunk must have
// exactly one owning object, or the link fails with a duplicate symbol).
#[test]
fn cross_tu_tbp_targets_submodule_procedure() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_tbp_targets_submodule_procedure count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("tb_mod.f90");
    let child_f90 = dir.join("tb_child.f90");
    let main_f90 = dir.join("tb_main.f90");
    let binary = dir.join("tb_bin");

    std::fs::write(
        &mod_f90,
        "module tb\n  implicit none\n  type :: counter\n    integer :: n = 0\n  contains\n    procedure :: bump\n  end type\n  interface\n    module subroutine bump(self, by)\n      class(counter), intent(inout) :: self\n      integer, intent(in) :: by\n    end subroutine\n  end interface\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        "submodule (tb) tb_impl\ncontains\n  module procedure bump\n    self%n = self%n + by\n  end procedure\nend submodule\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use tb\n  type(counter) :: c\n  call c%bump(5)\n  call c%bump(7)\n  if (c%n /= 12) error stop 1\n  print *, \"ok\"\nend program\n",
    )
    .unwrap();

    // Worst order again.
    let result = std::process::Command::new(&compiler)
        .current_dir(&dir)
        .args([
            child_f90.to_str().unwrap(),
            mod_f90.to_str().unwrap(),
            main_f90.to_str().unwrap(),
            "-o",
            binary.to_str().unwrap(),
        ])
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "cross-TU TBP→SMP build failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "cross-TU TBP→SMP wrong result:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// A parent module can emit the owning vtable before submodule procedure bodies
// are compiled. Concrete vtable slots must still point at those external
// module-procedure symbols; otherwise a wrapper that dispatches through the
// deferred binding lands on a null slot at runtime.
#[test]
fn parent_vtable_references_submodule_tbp_target() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=parent_vtable_references_submodule_tbp_target count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("vt_parent.f90");
    let child_f90 = dir.join("vt_child.f90");
    let main_f90 = dir.join("vt_main.f90");
    let mod_o = dir.join("vt_parent.o");
    let child_o = dir.join("vt_child.o");
    let main_o = dir.join("vt_main.o");
    let binary = dir.join("vt_bin");

    std::fs::write(
        &mod_f90,
        "module vt_parent\n  implicit none\n  type :: counter\n    integer :: n = 0\n  contains\n    procedure :: bump\n    procedure :: ensure\n  end type\n  interface\n    module subroutine bump(self, by)\n      class(counter), intent(inout) :: self\n      integer, intent(in) :: by\n    end subroutine\n  end interface\ncontains\n  subroutine ensure(self)\n    class(counter), intent(inout) :: self\n    call self%bump(5)\n  end subroutine\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &child_f90,
        "submodule (vt_parent) vt_child\ncontains\n  module procedure bump\n    self%n = self%n + by\n  end procedure\nend submodule\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use vt_parent\n  implicit none\n  type(counter) :: c\n  call c%ensure()\n  if (c%n /= 5) error stop 1\n  print *, \"ok\"\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &mod_f90, &mod_o, None);
    compile_file(&compiler, &child_f90, &child_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&mod_o, &child_o, &main_o], &binary);

    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "submodule-backed TBP vtable dispatch failed:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn local_child_vtable_keeps_imported_tbp_target_over_same_abi_interface_name() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=local_child_vtable_keeps_imported_tbp_target_over_same_abi_interface_name count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let rk_a_f90 = dir.join("rk_a.f90");
    let rk_b_f90 = dir.join("rk_b.f90");
    let facade_f90 = dir.join("facade.f90");
    let main_f90 = dir.join("main.f90");
    let rk_a_o = dir.join("rk_a.o");
    let rk_b_o = dir.join("rk_b.o");
    let facade_o = dir.join("facade.o");
    let main_o = dir.join("main.o");
    let binary = dir.join("facade_vtable_bin");

    std::fs::write(
        &rk_a_f90,
        "module rk_a\n  implicit none\n  type, abstract :: rk_class\n  contains\n    procedure(step_func), deferred :: step\n    procedure :: integrate => a_integrate\n  end type\n  abstract interface\n    subroutine step_func(self)\n      import :: rk_class\n      class(rk_class), intent(inout) :: self\n    end subroutine\n  end interface\n  type, extends(rk_class) :: rk8_10_class\n  contains\n    procedure :: step => rk8_10\n  end type\ncontains\n  subroutine a_integrate(self)\n    class(rk_class), intent(inout) :: self\n    call self%step()\n  end subroutine\n  subroutine rk8_10(self)\n    class(rk8_10_class), intent(inout) :: self\n  end subroutine\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &rk_b_f90,
        "module rk_b\n  implicit none\n  type, abstract :: other_class\n  contains\n    procedure(step_func), deferred :: step\n    procedure :: integrate => b_integrate\n  end type\n  abstract interface\n    subroutine step_func(self)\n      import :: other_class\n      class(other_class), intent(inout) :: self\n    end subroutine\n  end interface\ncontains\n  subroutine b_integrate(self)\n    class(other_class), intent(inout) :: self\n  end subroutine\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &facade_f90,
        "module facade\n  use rk_a\n  use rk_b\n  implicit none\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use facade\n  implicit none\n  type, extends(rk8_10_class) :: spacecraft\n    integer :: marker = 0\n  end type\n  type(spacecraft) :: s\n  call s%integrate()\n  print *, \"ok\"\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &rk_a_f90, &rk_a_o, None);
    compile_file(&compiler, &rk_b_f90, &rk_b_o, Some(&dir));
    compile_file(&compiler, &facade_f90, &facade_o, Some(&dir));
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));

    let undef = undefined_symbols(&main_o);
    assert!(
        !undef.iter().any(|sym| {
            sym.trim_start_matches('_')
                == "afs_modproc_rk_b_step_func"
        }),
        "local child vtable should keep rk_a's imported target, not rk_b's interface placeholder: {:?}",
        undef
    );

    link_files(&[&rk_a_o, &rk_b_o, &facade_o, &main_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "facade-imported child vtable dispatch failed:\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

// l08: vtable slot ordering must be identical whether a TU computes it
// from the type's source or from its `.amod`. The owner module dispatches
// `a()`/`b()` through `class(base)` (source-visible layout); the consumer
// dispatches the same calls on the same dynamic type seen only through
// the `.amod` (amod-only layout). The child overrides `a` (keeps the
// parent slot) and adds `c` (new slot), so a slot-order skew between the
// two views would call the wrong method and the two sums would diverge.
#[test]
fn cross_tu_vtable_slots_match_source_and_amod_views() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_vtable_slots_match_source_and_amod_views count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("vt_mod.f90");
    let main_f90 = dir.join("vt_main.f90");
    let binary = dir.join("vt_bin");

    std::fs::write(
        &mod_f90,
        "module vt\n\
         implicit none\n\
         type :: base\n\
         contains\n\
         procedure :: a => a_base\n\
         procedure :: b => b_base\n\
         end type\n\
         type, extends(base) :: child\n\
         contains\n\
         procedure :: a => a_child\n\
         procedure :: c => c_child\n\
         end type\n\
         contains\n\
         integer function a_base(self)\n\
         class(base), intent(in) :: self\n\
         a_base = 1\n\
         end function\n\
         integer function b_base(self)\n\
         class(base), intent(in) :: self\n\
         b_base = 2\n\
         end function\n\
         integer function a_child(self)\n\
         class(child), intent(in) :: self\n\
         a_child = 10\n\
         end function\n\
         integer function c_child(self)\n\
         class(child), intent(in) :: self\n\
         c_child = 30\n\
         end function\n\
         ! Source-visible dispatch: compiled in the owner TU.\n\
         integer function via_owner(x)\n\
         class(base), intent(in) :: x\n\
         via_owner = x%a() + x%b() * 100\n\
         end function\n\
         end module\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n\
         use vt\n\
         implicit none\n\
         class(base), allocatable :: s\n\
         integer :: owner_sum, consumer_sum\n\
         allocate(child :: s)\n\
         owner_sum = via_owner(s)            ! source-visible layout\n\
         consumer_sum = s%a() + s%b() * 100  ! amod-only layout\n\
         if (owner_sum /= 210) error stop 1\n\
         if (consumer_sum /= 210) error stop 2\n\
         print *, \"ok\"\n\
         end program\n",
    )
    .unwrap();

    let result = std::process::Command::new(&compiler)
        .current_dir(&dir)
        .args([
            mod_f90.to_str().unwrap(),
            main_f90.to_str().unwrap(),
            "-o",
            binary.to_str().unwrap(),
        ])
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "cross-TU vtable slot build failed:\n{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "cross-TU vtable slot ordering mismatch (source vs amod):\n{}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_polymorphic_copy_reports_unavailable_finalizer_context() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_polymorphic_copy_reports_unavailable_finalizer_context count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("dynamic_copy.f90");
    let main_f90 = dir.join("local_payload.f90");
    let mod_o = dir.join("dynamic_copy.o");
    let main_o = dir.join("local_payload.o");
    let binary = dir.join("dynamic_copy_bin");

    std::fs::write(
        &mod_f90,
        "module dynamic_copy_m\n  implicit none\ncontains\n  subroutine clone_value(source, target)\n    class(*), intent(in) :: source\n    class(*), allocatable, intent(out) :: target\n    target = source\n  end subroutine\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use dynamic_copy_m\n  implicit none\n  type :: payload_t\n    integer :: value = 1\n  contains\n    final :: finish\n  end type\n  type(payload_t) :: source\n  class(*), allocatable :: target\n  call clone_value(source, target)\ncontains\n  subroutine finish(item)\n    type(payload_t) :: item\n  end subroutine\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &mod_f90, &mod_o, None);
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&mod_o, &main_o], &binary);
    let result = Command::new(&binary)
        .output()
        .expect("binary launch failed");
    assert!(
        !result.status.success()
            && String::from_utf8_lossy(&result.stderr)
                .contains("polymorphic ownership cannot preserve a procedure-local FINAL binding"),
        "cross-TU dynamic copy did not report the unavailable FINAL context: status={:?} stdout={} stderr={}",
        result.status,
        String::from_utf8_lossy(&result.stdout),
        String::from_utf8_lossy(&result.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_class_base_source_and_mold_report_unavailable_finalizer_context() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_class_base_source_and_mold_report_unavailable_finalizer_context count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("dynamic_base_copy.f90");
    let mod_o = dir.join("dynamic_base_copy.o");

    std::fs::write(
        &mod_f90,
        "module dynamic_base_copy_m\n  implicit none\n  type :: base_t\n    integer :: value = 1\n  end type\n  type, extends(base_t) :: module_payload_t\n    integer, allocatable :: owned(:)\n  end type\ncontains\n  subroutine clone_source(source, target)\n    class(base_t), intent(in) :: source\n    class(base_t), allocatable, intent(out) :: target\n    allocate(target, source=source)\n  end subroutine\n  subroutine clone_mold(source, target)\n    class(base_t), intent(in) :: source\n    class(base_t), allocatable, intent(out) :: target\n    allocate(target, mold=source)\n  end subroutine\n  subroutine clone_array_source(source, target)\n    class(base_t), intent(in) :: source(:)\n    class(base_t), allocatable, intent(out) :: target(:)\n    allocate(target, source=source)\n  end subroutine\n  subroutine clone_bounded_mold(source, target)\n    class(base_t), intent(in) :: source(:)\n    class(base_t), allocatable, intent(out) :: target(:)\n    allocate(target(1), mold=source)\n  end subroutine\nend module\n",
    )
    .unwrap();
    compile_file(&compiler, &mod_f90, &mod_o, None);
    let mod_ir = dir.join("dynamic_base_copy.ir");
    let emit = Command::new(&compiler)
        .args([
            "--emit-ir",
            mod_f90.to_str().unwrap(),
            "-o",
            mod_ir.to_str().unwrap(),
        ])
        .output()
        .expect("module IR emission failed to spawn");
    assert!(
        emit.status.success(),
        "module IR emission failed: {}",
        String::from_utf8_lossy(&emit.stderr)
    );
    let ir = std::fs::read_to_string(&mod_ir).expect("cannot read module IR");
    let function_start = ir
        .find("func @afs_modproc_dynamic_base_copy_m_clone_bounded_mold")
        .expect("missing clone_bounded_mold IR");
    let function_tail = &ir[function_start..];
    let function_end = function_tail
        .find("\n  func @")
        .unwrap_or(function_tail.len());
    let function_ir = &function_tail[..function_end];
    let allocation = function_ir
        .find("call @afs_allocate_array")
        .expect("missing bounded MOLD allocation call");
    assert!(
        function_ir[..allocation]
            .matches("call @afs_error_stop_msg")
            .count()
            >= 2,
        "bounded polymorphic MOLD must validate both destination release and source metadata before allocation:\n{}",
        function_ir
    );

    let cases = [
        (
            "source",
            "  type(payload_t) :: source\n  class(base_t), allocatable :: target\n  allocate(source%owned(1))\n  call clone_source(source, target)\n",
        ),
        (
            "mold",
            "  type(payload_t) :: source\n  class(base_t), allocatable :: target\n  call clone_mold(source, target)\n",
        ),
        (
            "array_source",
            "  type(payload_t) :: source(1)\n  class(base_t), allocatable :: target(:)\n  allocate(source(1)%owned(1))\n  call clone_array_source(source, target)\n",
        ),
        (
            "zero_sized_mold",
            "  type(payload_t), allocatable :: source(:)\n  class(base_t), allocatable :: target(:)\n  allocate(source(0))\n  call clone_bounded_mold(source, target)\n  error stop 97\n",
        ),
    ];

    for (case, declarations_and_call) in cases {
        let main_f90 = dir.join(format!("local_payload_{}.f90", case));
        let main_o = dir.join(format!("local_payload_{}.o", case));
        let binary = dir.join(format!("dynamic_base_copy_{}_bin", case));
        let source = format!(
            "program p\n  use dynamic_base_copy_m\n  implicit none\n  type, extends(base_t) :: payload_t\n    integer, allocatable :: owned(:)\n  contains\n    final :: finish\n  end type\n{}contains\n  subroutine finish(item)\n    type(payload_t) :: item\n  end subroutine\nend program\n",
            declarations_and_call
        );
        std::fs::write(&main_f90, source).unwrap();
        compile_file(&compiler, &main_f90, &main_o, Some(&dir));
        link_files(&[&mod_o, &main_o], &binary);

        let result = Command::new(&binary)
            .output()
            .expect("binary launch failed");
        assert!(
            !result.status.success()
                && String::from_utf8_lossy(&result.stderr).contains(
                    "polymorphic ownership cannot preserve a procedure-local FINAL binding"
                ),
            "cross-TU CLASS(base) {} did not report the unavailable FINAL context: status={:?} stdout={} stderr={}",
            case,
            result.status,
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }

    let main_f90 = dir.join("module_payload_copy.f90");
    let main_o = dir.join("module_payload_copy.o");
    let binary = dir.join("module_payload_copy_bin");
    std::fs::write(
        &main_f90,
        "program p\n  use dynamic_base_copy_m\n  implicit none\n  type(module_payload_t) :: source(1)\n  class(base_t), allocatable :: target(:)\n  source(1)%owned = [4]\n  call clone_array_source(source, target)\n  source(1)%owned = [9]\n  select type (target)\n  type is (module_payload_t)\n    if (.not. allocated(target(1)%owned)) error stop 1\n    if (any(target(1)%owned /= [4])) error stop 2\n  class default\n    error stop 3\n  end select\n  print *, 'ok'\nend program\n",
    )
    .unwrap();
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&mod_o, &main_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "cross-TU CLASS(base) array SOURCE did not deep-copy module payload: {}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_zero_sized_class_base_assignment_reports_unavailable_finalizer_context() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_zero_sized_class_base_assignment_reports_unavailable_finalizer_context count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("dynamic_zero_assignment.f90");
    let main_f90 = dir.join("dynamic_zero_assignment_main.f90");
    let mod_o = dir.join("dynamic_zero_assignment.o");
    let main_o = dir.join("dynamic_zero_assignment_main.o");
    let binary = dir.join("dynamic_zero_assignment_bin");

    std::fs::write(
        &mod_f90,
        "module dynamic_zero_assignment_m\n  implicit none\n  type :: base_t\n    integer :: marker = 1\n  end type\ncontains\n  subroutine copy_zero(source, target)\n    class(base_t), intent(in) :: source(:)\n    class(base_t), allocatable, intent(out) :: target(:)\n    target = source\n  end subroutine\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use dynamic_zero_assignment_m\n  implicit none\n  type, extends(base_t) :: local_t\n    integer :: payload = 7\n  contains\n    final :: finish\n  end type\n  type(local_t), allocatable :: source(:)\n  class(base_t), allocatable :: target(:)\n  allocate(source(0))\n  call copy_zero(source, target)\n  error stop 97\ncontains\n  subroutine finish(item)\n    type(local_t) :: item\n  end subroutine\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &mod_f90, &mod_o, None);
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&mod_o, &main_o], &binary);
    let result = Command::new(&binary)
        .output()
        .expect("binary launch failed");
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        !result.status.success()
            && stderr.contains(
                "polymorphic ownership cannot preserve a procedure-local FINAL binding"
            )
            && !stderr.contains("ERROR STOP 97"),
        "zero-sized CLASS(base) assignment did not reject the unavailable FINAL context before ownership: status={:?} stdout={} stderr={}",
        result.status,
        String::from_utf8_lossy(&result.stdout),
        stderr
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_polymorphic_assignment_guards_existing_destination() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_polymorphic_assignment_guards_existing_destination count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("dynamic_replacement.f90");
    let main_f90 = dir.join("dynamic_replacement_main.f90");
    let mod_o = dir.join("dynamic_replacement.o");
    let main_o = dir.join("dynamic_replacement_main.o");
    let binary = dir.join("dynamic_replacement_bin");

    std::fs::write(
        &mod_f90,
        "module dynamic_replacement_m\n  implicit none\n  type :: base_t\n    integer :: marker = 1\n  contains\n    final :: finish_base\n  end type\n  type, extends(base_t) :: replacement_t\n    integer :: replacement = 22\n  end type\ncontains\n  subroutine finish_base(item)\n    type(base_t) :: item\n    print *, 'DESTINATION_FINALIZED', item%marker\n  end subroutine\n  subroutine replace_value(target)\n    class(base_t), allocatable, intent(inout) :: target\n    type(replacement_t) :: source\n    target = source\n  end subroutine\n  subroutine replace_array(target)\n    class(base_t), allocatable, intent(inout) :: target(:)\n    type(replacement_t) :: source(1)\n    target = source\n  end subroutine\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use dynamic_replacement_m\n  implicit none\n  type, extends(base_t) :: local_t\n    integer :: payload = 7\n  contains\n    final :: finish\n  end type\n  type(local_t), allocatable :: target\n  allocate(target)\n  call replace_value(target)\n  error stop 97\ncontains\n  subroutine finish(item)\n    type(local_t) :: item\n  end subroutine\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &mod_f90, &mod_o, None);
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&mod_o, &main_o], &binary);
    let result = Command::new(&binary)
        .output()
        .expect("binary launch failed");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        !result.status.success()
            && stderr.contains(
                "polymorphic ownership cannot preserve a procedure-local FINAL binding"
            )
            && !stderr.contains("ERROR STOP 97")
            && !stdout.contains("DESTINATION_FINALIZED"),
        "polymorphic assignment replaced a destination with an unavailable FINAL context: status={:?} stdout={} stderr={}",
        result.status,
        stdout,
        stderr
    );

    let array_main_f90 = dir.join("dynamic_replacement_array_main.f90");
    let array_main_o = dir.join("dynamic_replacement_array_main.o");
    let array_binary = dir.join("dynamic_replacement_array_bin");
    std::fs::write(
        &array_main_f90,
        "program p\n  use dynamic_replacement_m\n  implicit none\n  type, extends(base_t) :: local_t\n    integer :: payload = 7\n  contains\n    final :: finish\n  end type\n  type(local_t), allocatable :: target(:)\n  allocate(target(1))\n  call replace_array(target)\n  error stop 97\ncontains\n  subroutine finish(item)\n    type(local_t) :: item\n  end subroutine\nend program\n",
    )
    .unwrap();
    compile_file(&compiler, &array_main_f90, &array_main_o, Some(&dir));
    link_files(&[&mod_o, &array_main_o], &array_binary);
    let array_result = Command::new(&array_binary)
        .output()
        .expect("array binary launch failed");
    let array_stdout = String::from_utf8_lossy(&array_result.stdout);
    let array_stderr = String::from_utf8_lossy(&array_result.stderr);
    assert!(
        !array_result.status.success()
            && array_stderr.contains(
                "polymorphic ownership cannot preserve a procedure-local FINAL binding"
            )
            && !array_stderr.contains("ERROR STOP 97")
            && !array_stdout.contains("DESTINATION_FINALIZED"),
        "polymorphic array assignment replaced a destination with an unavailable FINAL context: status={:?} stdout={} stderr={}",
        array_result.status,
        array_stdout,
        array_stderr
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_recursive_class_base_scope_exit_guards_dynamic_lifecycle() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_recursive_class_base_scope_exit_guards_dynamic_lifecycle count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("recursive_owner.f90");
    let main_f90 = dir.join("recursive_scope_exit.f90");
    let mod_o = dir.join("recursive_owner.o");
    let main_o = dir.join("recursive_scope_exit.o");
    let binary = dir.join("recursive_scope_exit_bin");

    std::fs::write(
        &mod_f90,
        "module recursive_owner_m\n  implicit none\n  type :: base_t\n    integer :: marker = 1\n  end type\n  type :: node_t\n    class(base_t), allocatable :: value\n    type(node_t), allocatable :: next\n  end type\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use recursive_owner_m\n  implicit none\n  type, extends(base_t) :: local_t\n    integer :: payload = 7\n  contains\n    final :: finish\n  end type\n  block\n    type(node_t), allocatable :: root\n    allocate(root)\n    allocate(root%next)\n    allocate(local_t :: root%next%value)\n    print *, 'READY_FOR_SCOPE_EXIT'\n  end block\n  error stop 97\ncontains\n  subroutine finish(item)\n    type(local_t) :: item\n  end subroutine\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &mod_f90, &mod_o, None);
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&mod_o, &main_o], &binary);
    let result = Command::new(&binary)
        .output()
        .expect("binary launch failed");
    let stdout = String::from_utf8_lossy(&result.stdout);
    let stderr = String::from_utf8_lossy(&result.stderr);
    assert!(
        !result.status.success()
            && stderr.contains(
                "polymorphic ownership cannot preserve a procedure-local FINAL binding"
            )
            && !stderr.contains("ERROR STOP 97")
            && stdout.contains("READY_FOR_SCOPE_EXIT"),
        "recursive CLASS(base) scope cleanup bypassed dynamic lifecycle validation: status={:?} stdout={} stderr={}",
        result.status,
        stdout,
        stderr
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_explicit_bounds_preserve_dynamic_element_size() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_explicit_bounds_preserve_dynamic_element_size count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("dynamic_size.f90");
    let main_f90 = dir.join("dynamic_size_main.f90");
    let mod_o = dir.join("dynamic_size.o");
    let main_o = dir.join("dynamic_size_main.o");
    let binary = dir.join("dynamic_size_bin");

    std::fs::write(
        &mod_f90,
        "module dynamic_size_m\n  implicit none\n  type :: base_t\n    integer :: base = 1\n  end type\n  type, extends(base_t) :: payload_t\n    integer, allocatable :: owned(:)\n  end type\n  type :: holder_t\n    class(base_t), allocatable :: value(:, :)\n  end type\ncontains\n  subroutine clone_bounded_source(source, target)\n    class(base_t), intent(in) :: source(:)\n    class(base_t), allocatable, intent(out) :: target(:)\n    allocate(target(1), source=source)\n  end subroutine\n  subroutine clone_bounded_mold(source, target)\n    class(base_t), intent(in) :: source(:)\n    class(base_t), allocatable, intent(out) :: target(:)\n    allocate(target(1), mold=source)\n  end subroutine\n  subroutine clone_component_mold(source, holder, ok)\n    class(base_t), intent(in) :: source(:, :)\n    type(holder_t), intent(out) :: holder\n    integer, intent(out) :: ok\n    allocate(holder%value(2, 2), mold=source)\n    select type (value => holder%value)\n    type is (payload_t)\n      if (allocated(value(1, 1)%owned)) error stop 7\n      allocate(value(2, 2)%owned(2))\n      value(2, 2)%owned = [8, 9]\n      ok = merge(1, -1, all(value(2, 2)%owned == [8, 9]))\n    class default\n      ok = -2\n    end select\n  end subroutine\nend module\n",
    )
    .unwrap();
    std::fs::write(
        &main_f90,
        "program p\n  use dynamic_size_m\n  implicit none\n  type(payload_t) :: source(1), matrix(2, 2)\n  type(holder_t) :: holder\n  class(base_t), allocatable :: copied(:), shaped(:)\n  integer :: component_ok\n  source(1)%owned = [4, 5]\n  call clone_bounded_source(source, copied)\n  source(1)%owned = [9, 10]\n  select type (copied)\n  type is (payload_t)\n    if (.not. allocated(copied(1)%owned)) error stop 1\n    if (any(copied(1)%owned /= [4, 5])) error stop 2\n  class default\n    error stop 3\n  end select\n  call clone_bounded_mold(source, shaped)\n  select type (shaped)\n  type is (payload_t)\n    if (allocated(shaped(1)%owned)) error stop 4\n    allocate(shaped(1)%owned(2))\n    shaped(1)%owned = [6, 7]\n    if (any(shaped(1)%owned /= [6, 7])) error stop 5\n  class default\n    error stop 6\n  end select\n  call clone_component_mold(matrix, holder, component_ok)\n  if (component_ok /= 1) error stop 8\n  print *, 'ok'\nend program\n",
    )
    .unwrap();

    compile_file(&compiler, &mod_f90, &mod_o, None);
    compile_file(&compiler, &main_f90, &main_o, Some(&dir));
    link_files(&[&mod_o, &main_o], &binary);
    let output = run_binary(&binary);
    assert!(
        output.contains("ok"),
        "explicit-bound polymorphic allocation lost the dynamic element size: {}",
        output
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_polymorphic_release_reports_unavailable_finalizer_context() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_polymorphic_release_reports_unavailable_finalizer_context count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let mod_f90 = dir.join("dynamic_release.f90");
    let mod_o = dir.join("dynamic_release.o");

    std::fs::write(
        &mod_f90,
        "module dynamic_release_m\n  implicit none\n  type :: base_t\n    integer :: value = 1\n  end type\ncontains\n  subroutine clear_star(value)\n    class(*), allocatable, intent(out) :: value\n  end subroutine\n  subroutine clear_base(value)\n    class(base_t), allocatable, intent(out) :: value\n  end subroutine\n  subroutine clear_optional_star(value)\n    class(*), allocatable, optional, intent(out) :: value\n  end subroutine\n  subroutine clear_optional_base(value)\n    class(base_t), allocatable, optional, intent(out) :: value\n  end subroutine\n  subroutine release_star(value)\n    class(*), allocatable, intent(inout) :: value\n    deallocate(value)\n  end subroutine\n  subroutine release_base(value)\n    class(base_t), allocatable, intent(inout) :: value\n    deallocate(value)\n  end subroutine\nend module\n",
    )
    .unwrap();
    compile_file(&compiler, &mod_f90, &mod_o, None);

    let cases = [
        "clear_star",
        "clear_base",
        "clear_optional_star",
        "clear_optional_base",
        "release_star",
        "release_base",
    ];
    for procedure in cases {
        let main_f90 = dir.join(format!("{}_main.f90", procedure));
        let main_o = dir.join(format!("{}_main.o", procedure));
        let binary = dir.join(format!("{}_bin", procedure));
        let source = format!(
            "program p\n  use dynamic_release_m\n  implicit none\n  type, extends(base_t) :: payload_t\n    integer :: marker = 7\n  contains\n    final :: finish\n  end type\n  type(payload_t), allocatable :: value\n  allocate(value)\n  call {}(value)\n  error stop 99\ncontains\n  subroutine finish(item)\n    type(payload_t) :: item\n    if (item%marker < 0) error stop 98\n  end subroutine\nend program\n",
            procedure
        );
        std::fs::write(&main_f90, source).unwrap();
        compile_file(&compiler, &main_f90, &main_o, Some(&dir));
        link_files(&[&mod_o, &main_o], &binary);

        let result = Command::new(&binary)
            .output()
            .expect("binary launch failed");
        assert!(
            !result.status.success()
                && String::from_utf8_lossy(&result.stderr).contains(
                    "polymorphic ownership cannot preserve a procedure-local FINAL binding"
                ),
            "cross-TU {} did not report the unavailable FINAL context: status={:?} stdout={} stderr={}",
            procedure,
            result.status,
            String::from_utf8_lossy(&result.stdout),
            String::from_utf8_lossy(&result.stderr)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn cross_tu_class_star_intent_out_runs_dynamic_finalizers() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=cross_tu_class_star_intent_out_runs_dynamic_finalizers count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    multifile_test(
        "module dynamic_intent_out_m\n  implicit none\n  integer :: finalized = 0\n  integer :: observed_owned = 0\n  type :: payload_t\n    integer, allocatable :: owned(:)\n  contains\n    final :: finish\n  end type\ncontains\n  subroutine finish(item)\n    type(payload_t) :: item\n    finalized = finalized + 1\n    if (allocated(item%owned)) observed_owned = observed_owned + sum(item%owned)\n  end subroutine\n  subroutine clear_star(value)\n    class(*), allocatable, intent(out) :: value\n  end subroutine\n  subroutine clear_optional_star(value)\n    class(*), allocatable, optional, intent(out) :: value\n  end subroutine\nend module\n",
        "program p\n  use dynamic_intent_out_m\n  implicit none\n  type(payload_t), allocatable :: required, optional\n  allocate(required)\n  required%owned = [2, 3]\n  allocate(optional)\n  optional%owned = [5, 7]\n  call clear_star(required)\n  call clear_optional_star(optional)\n  if (allocated(required)) error stop 1\n  if (allocated(optional)) error stop 2\n  if (finalized /= 2) error stop 3\n  if (observed_owned /= 17) error stop 4\n  print *, 'intent-out-finalized', finalized, observed_owned\nend program\n",
        "intent-out-finalized 2 17",
    );
}

#[test]
fn enumeration_type_amod_roundtrip() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=enumeration_type_amod_roundtrip count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    // Orphaned l03 deferral (was l07's row): USE of a module that
    // defines an F2023 ENUMERATION TYPE must re-register the type and
    // its typed enumerator constants from the .amod — declaration,
    // assignment, NEXT, and HUGE all through the round-trip.
    multifile_test_flags(
        "module emod\n  implicit none\n  enumeration type :: color\n    enumerator :: red, green, blue\n  end enumeration type\nend module\n",
        "program p\n  use emod\n  implicit none\n  type(color) :: c\n  c = green\n  c = next(c)\n  print '(i0,1x,i0)', int(c), int(huge(c))\nend program\n",
        "3 3",
        &["--std=f2023"],
    );
}

#[test]
fn assumed_size_integer_dummy_cross_module_passes_data_address() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=assumed_size_integer_dummy_cross_module_passes_data_address count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    // A module procedure with an assumed-size `buf(*)` dummy passes its
    // argument by bare data address, not by descriptor. The `.amod`
    // records this correctly (no `descriptor` attr), but the consumer
    // reconstructs the dummy as AssumedShape (rank-based fallback), and
    // `descriptor_param_mask_for_lookup` used to let that lossy scope
    // reconstruction override the authoritative `.amod` mask — so the
    // caller passed a descriptor pointer the callee then read as data
    // (garbage element reads). Same-file callers were unaffected, which
    // is why every single-file fixture missed it.
    multifile_test(
        "module asize_i\n  implicit none\ncontains\n  integer function count_pos(buf) result(n)\n    integer, intent(in) :: buf(*)\n    n = 0\n    do while (buf(n + 1) /= 0)\n      n = n + 1\n    end do\n  end function\nend module\n",
        "program p\n  use asize_i\n  implicit none\n  integer :: a(6)\n  a = 0\n  a(1) = 7\n  a(2) = 8\n  a(3) = 9\n  print '(i0)', count_pos(a)\nend program\n",
        "3",
    );
}

#[test]
fn assumed_size_cchar_dummy_cross_module_reads_correctly() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=assumed_size_cchar_dummy_cross_module_reads_correctly count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    // The C-interop shape that crashed 7 fgof libraries: a NUL-terminator
    // scan over a `character(kind=c_char) :: buf(*)` assumed-size dummy in
    // a separately compiled module. Before the descriptor-mask fix the
    // caller passed a descriptor, the callee read garbage element values,
    // the scan overran, and the resulting bad length fed memmove and
    // SIGSEGV'd.
    multifile_test(
        "module asize_c\n  use iso_c_binding, only : c_char, c_null_char\n  implicit none\ncontains\n  integer function clen(buf) result(n)\n    character(kind=c_char), intent(in) :: buf(*)\n    n = 0\n    do while (buf(n + 1) /= c_null_char)\n      n = n + 1\n    end do\n  end function\nend module\n",
        "program p\n  use asize_c\n  use iso_c_binding, only : c_char, c_null_char\n  implicit none\n  character(kind=c_char) :: b(8)\n  integer :: i\n  do i = 1, 8\n    b(i) = c_null_char\n  end do\n  b(1) = 'x'\n  b(2) = 'y'\n  b(3) = 'z'\n  print '(i0)', clen(b)\nend program\n",
        "3",
    );
}

#[test]
fn generic_dispatch_allocatable_rank2_component_cross_module() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=generic_dispatch_allocatable_rank2_component_cross_module count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    // The stdlib sparse `sort_coo(COO%index, ...)` regression: dispatching a
    // generic on an allocatable rank-2 derived-type component whose type is
    // defined in a separately compiled module. A deferred-shape component's
    // `.amod` layout carried empty dims, so its declared rank (2) was lost;
    // the actual reported rank 1 and no rank-2 specific matched. Fixed by
    // seeding a deferred-shape component's dims with `vec![(1, 0); rank]` so
    // `dims.len()` preserves the declared rank. Correct dispatch binds the
    // 4-arg specific: 5 + 7 + 0 = 12.
    multifile_test(
        "module gdx_types\n  implicit none\n  integer, parameter :: ilp = 4\n  type :: base_t\n    integer(ilp) :: nrows = 0, ncols = 0, nnz = 0\n  end type\n  type, extends(base_t) :: coo_t\n    integer(ilp), allocatable :: index(:,:)\n  end type\nend module\nmodule gdx_ops\n  use gdx_types\n  implicit none\n  interface sort_coo\n    module procedure sort4\n    module procedure sort5\n  end interface\ncontains\n  subroutine sort4(a, n, num_rows, num_cols)\n    integer(ilp), intent(inout) :: a(2,*)\n    integer(ilp), intent(inout) :: n\n    integer(ilp), intent(in) :: num_rows, num_cols\n    n = num_rows + num_cols + a(1,1)\n  end subroutine\n  subroutine sort5(a, data, n, num_rows, num_cols)\n    integer(ilp), intent(inout) :: a(2,*)\n    real, intent(inout) :: data(*)\n    integer(ilp), intent(inout) :: n\n    integer(ilp), intent(in) :: num_rows, num_cols\n    n = num_rows\n  end subroutine\nend module\n",
        "program p\n  use gdx_types\n  use gdx_ops\n  implicit none\n  type(coo_t) :: c\n  allocate(c%index(2,10))\n  c%index = 0\n  c%nnz = 3\n  c%nrows = 5\n  c%ncols = 7\n  call sort_coo(c%index, c%nnz, c%nrows, c%ncols)\n  print '(i0)', c%nnz\nend program\n",
        "12",
    );
}

#[test]
fn generic_dispatch_block_local_scalar_not_shadowed_by_foreign_dummy_rank() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=generic_dispatch_block_local_scalar_not_shadowed_by_foreign_dummy_rank count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    // The order-dependent stdlib `dense(A)` / `check(...)` regression. A
    // scalar derived-type actual `a` declared inside a BLOCK isn't in the
    // procedure scope, so the generic dispatcher's rank cross-check fell
    // through to a whole-symbol-table lookup and picked up a same-named
    // rank-1 dummy from the use-associated noise_mod — inferring the scalar
    // actual as rank 1 and matching no rank-0 specific. Which foreign `a`
    // won depended on module load order. The rank cross-check for a known
    // local now stays in the current scope. Correct dispatch: 5 + 100.
    multifile_test(
        "module noise_mod\n  implicit none\ncontains\n  subroutine noise(a)\n    integer, intent(inout) :: a(:)\n    a = a + 1\n  end subroutine\nend module\nmodule wt_mod\n  implicit none\n  type :: wt\n    integer :: v = 0\n  end type\n  interface widen\n    module procedure widen_t\n  end interface\ncontains\n  integer function widen_t(a) result(r)\n    type(wt), intent(in) :: a\n    r = a%v + 100\n  end function\nend module\n",
        "program p\n  use noise_mod\n  use wt_mod\n  implicit none\n  block\n    type(wt) :: a\n    a%v = 5\n    print '(i0)', widen(a)\n  end block\nend program\n",
        "105",
    );
}

#[test]
fn elemental_procedure_pointer_interfaces_are_rejected_before_artifact_publication() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile test=elemental_procedure_pointer_interfaces_are_rejected_before_artifact_publication count=2 reason=\"{}\"",
            reason
        );
        return;
    }

    let compiler = find_compiler();
    let dir = unique_dir();
    let invalid_source = dir.join("invalid_elemental_pointer.f90");
    let invalid_object = dir.join("invalid_elemental_pointer.o");
    let invalid_amod = dir.join("invalid_elemental_pointer.amod");
    std::fs::write(
        &invalid_source,
        r#"module invalid_elemental_pointer
  implicit none
  abstract interface
    elemental integer function callback(value)
      integer, intent(in) :: value
    end function callback
  end interface
  procedure(callback), pointer :: handler
end module invalid_elemental_pointer
"#,
    )
    .unwrap();

    let invalid = Command::new(&compiler)
        .current_dir(&dir)
        .arg(&invalid_source)
        .args(["-c", "-o"])
        .arg(&invalid_object)
        .output()
        .expect("invalid elemental procedure-pointer compile failed to spawn");
    assert!(!invalid.status.success(), "invalid declaration compiled");
    let invalid_stderr = String::from_utf8_lossy(&invalid.stderr);
    assert!(
        invalid_stderr.contains("procedure pointer 'handler' may not have an ELEMENTAL interface"),
        "invalid declaration produced the wrong diagnostic:\n{invalid_stderr}"
    );
    assert!(
        !invalid_object.exists() && !invalid_amod.exists(),
        "failed module declaration published an object or .amod"
    );

    let provider_source = dir.join("elemental_api.f90");
    let provider_object = dir.join("elemental_api.o");
    std::fs::write(
        &provider_source,
        r#"module elemental_api
  implicit none
  private
  public :: callback
  abstract interface
    elemental integer function callback(value)
      integer, intent(in) :: value
    end function callback
  end interface
end module elemental_api
"#,
    )
    .unwrap();
    compile_file(&compiler, &provider_source, &provider_object, None);
    assert!(
        dir.join("elemental_api.amod").exists(),
        "provider did not publish its module interface"
    );

    let consumer_source = dir.join("elemental_consumer.f90");
    let consumer_object = dir.join("elemental_consumer.o");
    std::fs::write(
        &consumer_source,
        r#"program elemental_consumer
  use elemental_api, only: callback
  implicit none
  procedure(callback), pointer :: handler
end program elemental_consumer
"#,
    )
    .unwrap();
    let consumer = Command::new(&compiler)
        .current_dir(&dir)
        .arg(&consumer_source)
        .args(["-c", "-o"])
        .arg(&consumer_object)
        .arg(format!("-I{}", dir.display()))
        .output()
        .expect("elemental .amod consumer compile failed to spawn");
    assert!(
        !consumer.status.success(),
        "consumer accepted an imported elemental procedure-pointer interface"
    );
    let consumer_stderr = String::from_utf8_lossy(&consumer.stderr);
    assert!(
        consumer_stderr.contains("procedure pointer 'handler' may not have an ELEMENTAL interface"),
        "imported interface produced the wrong diagnostic:\n{consumer_stderr}"
    );
    assert!(
        !consumer_object.exists(),
        "failed .amod consumer published an object"
    );

    let _ = std::fs::remove_dir_all(&dir);
}
