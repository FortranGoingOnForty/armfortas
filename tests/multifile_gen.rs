//! Generated multi-file dependency chain tests.
//!
//! Programmatically builds N-module chains, diamond patterns, and trees,
//! compiles each to .o in dependency order, links, runs, and verifies
//! that symbols propagate through deep dependency chains.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

fn unique_dir(prefix: &str) -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir =
        std::env::temp_dir().join(format!("afs_gen_{}_{}_{}", prefix, std::process::id(), id));
    fs::create_dir_all(&dir).unwrap();
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

fn compile_file(compiler: &Path, source: &Path, output: &Path, search_dir: &Path, opt: &str) {
    // -J{dir} writes generated .amod files into the test's unique
    // temp dir. Without it, armfortas writes .amod to the process's
    // current working directory (the repo root), where parallel
    // multifile tests race on the same `.amod` filename and one
    // test reads another's (or a mid-write empty) artifact.
    let result = Command::new(compiler)
        .args([
            source.to_str().unwrap(),
            "-c",
            opt,
            "-o",
            output.to_str().unwrap(),
            &format!("-I{}", search_dir.display()),
            &format!("-J{}", search_dir.display()),
        ])
        .output()
        .expect("compiler launch failed");
    assert!(
        result.status.success(),
        "compile {} failed:\n{}",
        source.display(),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn link_files(objects: &[PathBuf], output: &Path) {
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

// ---- Generators ----

/// Generate a linear chain of N modules:
///   M1 uses M2, M2 uses M3, ..., M_N uses nothing.
/// Each module defines a PARAMETER that references the previous.
/// The main program prints the final accumulated value.
fn gen_chain(depth: usize) -> (Vec<(String, String)>, String, &'static str) {
    let mut files = Vec::new();

    // Module N (leaf) — no dependencies.
    files.push((
        format!("mod_{}.f90", depth),
        format!(
            "module mod_{n}\n  implicit none\n  integer, parameter :: val_{n} = {n}\nend module\n",
            n = depth
        ),
    ));

    // Modules N-1 down to 1 — each uses the next.
    for i in (1..depth).rev() {
        files.push((
            format!("mod_{}.f90", i),
            format!(
                "module mod_{i}\n  use mod_{next}\n  implicit none\n  \
                 integer, parameter :: val_{i} = val_{next} + {i}\nend module\n",
                i = i,
                next = i + 1
            ),
        ));
    }

    // Main program uses mod_1 and prints the accumulated value.
    let expected: usize = (1..=depth).sum();
    let main_src =
        format!("program p\n  use mod_1\n  implicit none\n  print *, val_1\nend program\n");

    // Files are already in compilation order: leaf first, then towards root.
    let expected_str = Box::leak(format!("{}", expected).into_boxed_str());
    (files, main_src, expected_str)
}

/// Generate a diamond: A uses B1..B_width, each Bi uses C.
/// C defines a base value, each Bi adds its index, A sums them.
fn gen_diamond(width: usize) -> (Vec<(String, String)>, String, &'static str) {
    let mut files = Vec::new();

    // C (leaf).
    files.push((
        "mod_c.f90".into(),
        "module mod_c\n  implicit none\n  integer, parameter :: base = 100\nend module\n".into(),
    ));

    // B1..B_width.
    for i in 1..=width {
        files.push((
            format!("mod_b{}.f90", i),
            format!(
                "module mod_b{i}\n  use mod_c\n  implicit none\n  \
                 integer, parameter :: val_b{i} = base + {i}\nend module\n",
                i = i
            ),
        ));
    }

    // Main program uses all Bi and prints the sum.
    let use_stmts: String = (1..=width)
        .map(|i| format!("  use mod_b{}", i))
        .collect::<Vec<_>>()
        .join("\n");
    let sum_expr: String = (1..=width)
        .map(|i| format!("val_b{}", i))
        .collect::<Vec<_>>()
        .join(" + ");
    let main_src = format!(
        "program p\n{}\n  implicit none\n  print *, {}\nend program\n",
        use_stmts, sum_expr
    );

    let expected: usize = (1..=width).map(|i| 100 + i).sum();
    let expected_str = Box::leak(format!("{}", expected).into_boxed_str());
    (files, main_src, expected_str)
}

fn run_generated_test(
    files: Vec<(String, String)>,
    main_src: String,
    expected: &str,
    opt: &str,
    label: &str,
) {
    let compiler = find_compiler();
    let dir = unique_dir(label);

    // Write all module files.
    for (name, src) in &files {
        fs::write(dir.join(name), src).unwrap();
    }
    fs::write(dir.join("main.f90"), &main_src).unwrap();

    // Compile modules in order (they're already in dependency order).
    let mut objects = Vec::new();
    for (name, _) in &files {
        let f90 = dir.join(name);
        let stem = name.trim_end_matches(".f90");
        let obj = dir.join(format!("{}.o", stem));
        compile_file(&compiler, &f90, &obj, &dir, opt);
        objects.push(obj);
    }

    // Compile main.
    let main_o = dir.join("main.o");
    compile_file(&compiler, &dir.join("main.f90"), &main_o, &dir, opt);
    objects.push(main_o);

    // Link and run.
    let binary = dir.join("test_bin");
    link_files(&objects, &binary);
    let output = run_binary(&binary);

    assert!(
        output.contains(expected),
        "{} [{}]: expected '{}' in output, got:\n{}",
        label,
        opt,
        expected,
        output
    );

    let _ = fs::remove_dir_all(&dir);
}

// ---- Tests ----

#[test]
fn chain_depth_5() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile_gen test=chain_depth_5 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let (files, main_src, expected) = gen_chain(5);
    run_generated_test(files, main_src, expected, "-O0", "chain5");
}

#[test]
fn chain_depth_10() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile_gen test=chain_depth_10 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let (files, main_src, expected) = gen_chain(10);
    run_generated_test(files, main_src, expected, "-O0", "chain10");
}

#[test]
fn chain_depth_20() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile_gen test=chain_depth_20 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let (files, main_src, expected) = gen_chain(20);
    run_generated_test(files, main_src, expected, "-O0", "chain20");
}

#[test]
fn chain_depth_10_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O2") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile_gen test=chain_depth_10_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let (files, main_src, expected) = gen_chain(10);
    run_generated_test(files, main_src, expected, "-O2", "chain10_o2");
}

#[test]
fn diamond_width_4() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile_gen test=diamond_width_4 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let (files, main_src, expected) = gen_diamond(4);
    run_generated_test(files, main_src, expected, "-O0", "diamond4");
}

#[test]
fn diamond_width_8() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile_gen test=diamond_width_8 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let (files, main_src, expected) = gen_diamond(8);
    run_generated_test(files, main_src, expected, "-O0", "diamond8");
}

#[test]
fn diamond_width_4_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O2") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile_gen test=diamond_width_4_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let (files, main_src, expected) = gen_diamond(4);
    run_generated_test(files, main_src, expected, "-O2", "diamond4_o2");
}

// ---- Cross-optimization ABI matrix tests ----

/// Compile modules at one opt level, main at another, link together.
/// Verifies ABI consistency across optimization levels.
fn run_cross_opt_test(
    files: Vec<(String, String)>,
    main_src: String,
    expected: &str,
    mod_opt: &str,
    main_opt: &str,
    label: &str,
) {
    let compiler = find_compiler();
    let dir = unique_dir(label);

    for (name, src) in &files {
        fs::write(dir.join(name), src).unwrap();
    }
    fs::write(dir.join("main.f90"), &main_src).unwrap();

    // Compile modules at mod_opt.
    let mut objects = Vec::new();
    for (name, _) in &files {
        let f90 = dir.join(name);
        let stem = name.trim_end_matches(".f90");
        let obj = dir.join(format!("{}.o", stem));
        compile_file(&compiler, &f90, &obj, &dir, mod_opt);
        objects.push(obj);
    }

    // Compile main at main_opt.
    let main_o = dir.join("main.o");
    compile_file(&compiler, &dir.join("main.f90"), &main_o, &dir, main_opt);
    objects.push(main_o);

    let binary = dir.join("test_bin");
    link_files(&objects, &binary);
    let output = run_binary(&binary);

    assert!(
        output.contains(expected),
        "{}: mod@{} + main@{}: expected '{}' in output, got:\n{}",
        label,
        mod_opt,
        main_opt,
        expected,
        output
    );

    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn abi_matrix_chain5_mod_o0_main_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile_gen test=abi_matrix_chain5_mod_o0_main_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let (files, main_src, expected) = gen_chain(5);
    run_cross_opt_test(files, main_src, expected, "-O0", "-O2", "abi_chain5_0_2");
}

#[test]
fn abi_matrix_chain5_mod_o2_main_o0() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O2") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile_gen test=abi_matrix_chain5_mod_o2_main_o0 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let (files, main_src, expected) = gen_chain(5);
    run_cross_opt_test(files, main_src, expected, "-O2", "-O0", "abi_chain5_2_0");
}

#[test]
fn abi_matrix_diamond4_mod_o0_main_o2() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile_gen test=abi_matrix_diamond4_mod_o0_main_o2 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let (files, main_src, expected) = gen_diamond(4);
    run_cross_opt_test(files, main_src, expected, "-O0", "-O2", "abi_dia4_0_2");
}

#[test]
fn abi_matrix_diamond4_mod_o2_main_o0() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O2") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile_gen test=abi_matrix_diamond4_mod_o2_main_o0 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let (files, main_src, expected) = gen_diamond(4);
    run_cross_opt_test(files, main_src, expected, "-O2", "-O0", "abi_dia4_2_0");
}

#[test]
fn abi_matrix_chain5_mod_o0_main_ofast() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-O0") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile_gen test=abi_matrix_chain5_mod_o0_main_ofast count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let (files, main_src, expected) = gen_chain(5);
    run_cross_opt_test(
        files,
        main_src,
        expected,
        "-O0",
        "-Ofast",
        "abi_chain5_0_fast",
    );
}

#[test]
fn abi_matrix_chain5_mod_ofast_main_o0() {
    if let Err(reason) = armfortas::testing::native_e2e_level_support("-Ofast") {
        eprintln!(
            "\nHARNESS_SKIP suite=multifile_gen test=abi_matrix_chain5_mod_ofast_main_o0 count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let (files, main_src, expected) = gen_chain(5);
    run_cross_opt_test(
        files,
        main_src,
        expected,
        "-Ofast",
        "-O0",
        "abi_chain5_fast_0",
    );
}
