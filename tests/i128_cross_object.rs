use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::time::SystemTime;

use armfortas::driver::{compile, OptLevel, Options};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);
static CROSS_OBJECT_LOCK: Mutex<()> = Mutex::new(());

fn fixture(name: &str) -> PathBuf {
    let path = PathBuf::from("tests/fixtures").join(name);
    assert!(path.exists(), "missing test fixture {}", path.display());
    path
}

fn unique_temp_path(prefix: &str, stem: &str, ext: &str) -> PathBuf {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!(
        "afs_{}_{}_{}_{}{}",
        prefix,
        std::process::id(),
        id,
        stem,
        ext
    ))
}

fn compile_fortran_object(source: &Path, output: &Path, opt_level: OptLevel) {
    let opts = Options {
        input: source.to_path_buf(),
        output: Some(output.to_path_buf()),
        emit_obj: true,
        opt_level,
        ..Options::default()
    };
    compile(&opts).unwrap_or_else(|e| {
        panic!(
            "armfortas object compile failed for {}:\n{}",
            source.display(),
            e
        )
    });
}

fn opt_label(opt_level: OptLevel) -> &'static str {
    match opt_level {
        OptLevel::O0 => "o0",
        OptLevel::O1 => "o1",
        OptLevel::O2 => "o2",
        OptLevel::O3 => "o3",
        OptLevel::Os => "os",
        OptLevel::Ofast => "ofast",
    }
}

fn compile_c_object(source: &Path, output: &Path) {
    let output_status = Command::new("clang")
        .args([
            "-arch",
            "arm64",
            "-c",
            source.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .output()
        .expect("cannot launch clang");
    assert!(
        output_status.status.success(),
        "clang failed for {}:\n{}",
        source.display(),
        String::from_utf8_lossy(&output_status.stderr)
    );
}

fn find_runtime_lib() -> PathBuf {
    let workspace_root = PathBuf::from(".");
    maybe_refresh_runtime_lib(&workspace_root);
    for candidate in [
        workspace_root.join("target/debug/libarmfortas_rt.a"),
        workspace_root.join("target/release/libarmfortas_rt.a"),
    ] {
        if candidate.exists() {
            return candidate;
        }
    }
    panic!("cannot find libarmfortas_rt.a — build with `cargo build -p armfortas-rt` first");
}

fn maybe_refresh_runtime_lib(workspace_root: &Path) {
    let runtime_dir = workspace_root.join("runtime");
    if !runtime_dir.join("Cargo.toml").exists() {
        return;
    }

    let Some(source_mtime) = newest_mtime(&runtime_dir) else {
        return;
    };
    let debug_archive = workspace_root.join("target/debug/libarmfortas_rt.a");
    let archive_mtime = fs::metadata(&debug_archive).ok().and_then(|meta| meta.modified().ok());

    if archive_mtime.is_some_and(|mtime| mtime >= source_mtime) {
        return;
    }

    let cargo = std::env::var("CARGO").unwrap_or_else(|_| "cargo".into());
    let output = Command::new(cargo)
        .current_dir(workspace_root)
        .args(["build", "-p", "armfortas-rt"])
        .output()
        .expect("cannot rebuild libarmfortas_rt.a");
    assert!(
        output.status.success(),
        "cannot rebuild libarmfortas_rt.a:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
}

fn newest_mtime(path: &Path) -> Option<SystemTime> {
    let meta = fs::metadata(path).ok()?;
    let mut newest = meta.modified().ok()?;
    if meta.is_dir() {
        for entry in fs::read_dir(path).ok()? {
            let entry = entry.ok()?;
            let child = newest_mtime(&entry.path())?;
            if child > newest {
                newest = child;
            }
        }
    }
    Some(newest)
}

fn sdk_root() -> String {
    let output = Command::new("xcrun")
        .args(["--show-sdk-path"])
        .output()
        .expect("cannot run xcrun");
    assert!(
        output.status.success(),
        "xcrun failed:\n{}",
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).trim().to_string()
}

fn link_objects(objects: &[&Path], output: &Path) {
    let rt_path = find_runtime_lib();
    let sysroot = sdk_root();
    let mut args: Vec<String> = objects
        .iter()
        .map(|path| path.to_string_lossy().into_owned())
        .collect();
    args.push(rt_path.to_string_lossy().into_owned());
    args.extend([
        "-lSystem".into(),
        "-no_uuid".into(),
        "-syslibroot".into(),
        sysroot,
        "-e".into(),
        "_main".into(),
        "-o".into(),
        output.to_string_lossy().into_owned(),
    ]);

    let output_status = Command::new("ld")
        .args(&args)
        .output()
        .expect("cannot launch ld");
    assert!(
        output_status.status.success(),
        "ld failed:\n{}",
        String::from_utf8_lossy(&output_status.stderr)
    );
}

fn run_binary(binary: &Path) -> std::process::Output {
    let sandbox = unique_temp_path("i128_cross_object_run", "sandbox", "");
    fs::create_dir_all(&sandbox).expect("cannot create run sandbox");
    let output = Command::new(binary)
        .current_dir(&sandbox)
        .output()
        .unwrap_or_else(|e| panic!("cannot run {}: {}", binary.display(), e));
    let _ = fs::remove_dir_all(&sandbox);
    output
}

fn tool_output(tool: &str, args: &[&str]) -> String {
    let output = Command::new(tool)
        .args(args)
        .output()
        .unwrap_or_else(|e| panic!("cannot run {}: {}", tool, e));
    assert!(
        output.status.success(),
        "{} failed:\n{}",
        tool,
        String::from_utf8_lossy(&output.stderr)
    );
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn run_cross_object_case_named(
    program_name: &str,
    helper_name: &str,
    opt_level: OptLevel,
    expected_score: char,
) {
    let _guard = CROSS_OBJECT_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let program = fixture(program_name);
    let helper = fixture(helper_name);
    let stem = program.file_stem().unwrap().to_str().unwrap();
    let fortran_obj = unique_temp_path("i128_cross_object", &format!("{}_{}", stem, opt_label(opt_level)), ".o");
    let helper_obj = unique_temp_path("i128_cross_object_helper", stem, ".o");
    let binary = unique_temp_path("i128_cross_object_bin", &format!("{}_{}", stem, opt_label(opt_level)), "");

    compile_fortran_object(&program, &fortran_obj, opt_level);
    compile_c_object(&helper, &helper_obj);
    link_objects(&[&fortran_obj, &helper_obj], &binary);

    let run = run_binary(&binary);
    assert_eq!(
        run.status.code().unwrap_or(-1),
        0,
        "cross-object integer(16) binary should exit successfully:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains(expected_score),
        "cross-object integer(16) binary should print score {}:\n{}",
        expected_score,
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = fs::remove_file(&fortran_obj);
    let _ = fs::remove_file(&helper_obj);
    let _ = fs::remove_file(&binary);
}

fn run_cross_object_case(opt_level: OptLevel) {
    run_cross_object_case_named(
        "integer16_external_call.f90",
        "integer16_external_call_helper.c",
        opt_level,
        '1',
    );
}

fn deterministic_cross_object_case_named(program_name: &str, helper_name: &str, opt_level: OptLevel) {
    let _guard = CROSS_OBJECT_LOCK.lock().unwrap_or_else(|poison| poison.into_inner());
    let program = fixture(program_name);
    let helper = fixture(helper_name);
    let stem = program.file_stem().unwrap().to_str().unwrap();
    let fortran_obj = unique_temp_path("i128_cross_object", &format!("{}_{}", stem, opt_label(opt_level)), ".o");
    let helper_obj = unique_temp_path("i128_cross_object_helper", stem, ".o");
    let binary = unique_temp_path("i128_cross_object_bin", &format!("{}_{}", stem, opt_label(opt_level)), "");

    compile_fortran_object(&program, &fortran_obj, opt_level);
    compile_c_object(&helper, &helper_obj);

    link_objects(&[&fortran_obj, &helper_obj], &binary);
    let load_commands = tool_output("otool", &["-l", binary.to_str().unwrap()]);
    assert!(
        !load_commands.contains("LC_UUID"),
        "linked cross-object integer(16) binary should omit LC_UUID:\n{}",
        load_commands
    );
    let first = fs::read(&binary).expect("cannot read first linked binary");

    link_objects(&[&fortran_obj, &helper_obj], &binary);
    let second = fs::read(&binary).expect("cannot read second linked binary");

    assert_eq!(
        first, second,
        "linked cross-object integer(16) binary should be byte-identical across rebuilds"
    );

    let _ = fs::remove_file(&fortran_obj);
    let _ = fs::remove_file(&helper_obj);
    let _ = fs::remove_file(&binary);
}

fn deterministic_cross_object_case(opt_level: OptLevel) {
    deterministic_cross_object_case_named(
        "integer16_external_call.f90",
        "integer16_external_call_helper.c",
        opt_level,
    );
}

#[test]
fn external_i128_call_runs_across_objects_at_o0() {
    run_cross_object_case(OptLevel::O0);
}

#[test]
fn external_i128_call_runs_across_objects_at_o1() {
    run_cross_object_case(OptLevel::O1);
}

#[test]
fn external_i128_call_runs_across_objects_at_o2() {
    run_cross_object_case(OptLevel::O2);
}

#[test]
fn external_i128_call_runs_across_objects_at_o3() {
    run_cross_object_case(OptLevel::O3);
}

#[test]
fn external_i128_call_runs_across_objects_at_os() {
    run_cross_object_case(OptLevel::Os);
}

#[test]
fn external_i128_call_runs_across_objects_at_ofast() {
    run_cross_object_case(OptLevel::Ofast);
}

#[test]
fn linked_external_i128_binary_is_deterministic_at_o0() {
    deterministic_cross_object_case(OptLevel::O0);
}

#[test]
fn linked_external_i128_binary_is_deterministic_at_o1() {
    deterministic_cross_object_case(OptLevel::O1);
}

#[test]
fn linked_external_i128_binary_is_deterministic_at_o2() {
    deterministic_cross_object_case(OptLevel::O2);
}

#[test]
fn linked_external_i128_binary_is_deterministic_at_o3() {
    deterministic_cross_object_case(OptLevel::O3);
}

#[test]
fn linked_external_i128_binary_is_deterministic_at_os() {
    deterministic_cross_object_case(OptLevel::Os);
}

#[test]
fn linked_external_i128_binary_is_deterministic_at_ofast() {
    deterministic_cross_object_case(OptLevel::Ofast);
}

#[test]
fn external_stack_i128_call_runs_across_objects_at_o0() {
    run_cross_object_case_named(
        "integer16_external_stack_call.f90",
        "integer16_external_stack_call_helper.c",
        OptLevel::O0,
        '1',
    );
}

#[test]
fn external_stack_i128_call_runs_across_objects_at_o1() {
    run_cross_object_case_named(
        "integer16_external_stack_call.f90",
        "integer16_external_stack_call_helper.c",
        OptLevel::O1,
        '1',
    );
}

#[test]
fn external_stack_i128_call_runs_across_objects_at_o2() {
    run_cross_object_case_named(
        "integer16_external_stack_call.f90",
        "integer16_external_stack_call_helper.c",
        OptLevel::O2,
        '1',
    );
}

#[test]
fn external_stack_i128_call_runs_across_objects_at_o3() {
    run_cross_object_case_named(
        "integer16_external_stack_call.f90",
        "integer16_external_stack_call_helper.c",
        OptLevel::O3,
        '1',
    );
}

#[test]
fn external_stack_i128_call_runs_across_objects_at_os() {
    run_cross_object_case_named(
        "integer16_external_stack_call.f90",
        "integer16_external_stack_call_helper.c",
        OptLevel::Os,
        '1',
    );
}

#[test]
fn external_stack_i128_call_runs_across_objects_at_ofast() {
    run_cross_object_case_named(
        "integer16_external_stack_call.f90",
        "integer16_external_stack_call_helper.c",
        OptLevel::Ofast,
        '1',
    );
}

#[test]
fn linked_external_stack_i128_binary_is_deterministic_at_o0() {
    deterministic_cross_object_case_named(
        "integer16_external_stack_call.f90",
        "integer16_external_stack_call_helper.c",
        OptLevel::O0,
    );
}

#[test]
fn linked_external_stack_i128_binary_is_deterministic_at_o1() {
    deterministic_cross_object_case_named(
        "integer16_external_stack_call.f90",
        "integer16_external_stack_call_helper.c",
        OptLevel::O1,
    );
}

#[test]
fn linked_external_stack_i128_binary_is_deterministic_at_o2() {
    deterministic_cross_object_case_named(
        "integer16_external_stack_call.f90",
        "integer16_external_stack_call_helper.c",
        OptLevel::O2,
    );
}

#[test]
fn linked_external_stack_i128_binary_is_deterministic_at_o3() {
    deterministic_cross_object_case_named(
        "integer16_external_stack_call.f90",
        "integer16_external_stack_call_helper.c",
        OptLevel::O3,
    );
}

#[test]
fn linked_external_stack_i128_binary_is_deterministic_at_os() {
    deterministic_cross_object_case_named(
        "integer16_external_stack_call.f90",
        "integer16_external_stack_call_helper.c",
        OptLevel::Os,
    );
}

#[test]
fn linked_external_stack_i128_binary_is_deterministic_at_ofast() {
    deterministic_cross_object_case_named(
        "integer16_external_stack_call.f90",
        "integer16_external_stack_call_helper.c",
        OptLevel::Ofast,
    );
}
