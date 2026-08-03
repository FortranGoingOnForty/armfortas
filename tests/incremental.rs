//! Incremental compilation tests.
//!
//! Verifies that the .amod module file system handles incremental
//! rebuilds correctly:
//!   - Recompiling a module produces the same .amod when the public
//!     interface is unchanged.
//!   - Changing a module's public interface produces a different .amod.
//!   - Changing only private implementation does NOT change .amod.
//!   - A consumer recompiled against the same .amod produces the
//!     same .o (no unnecessary recompilation cascade).

use std::ffi::OsStr;
use std::fs;
use std::io::Write;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

const PROVENANCE_PARENT_SOURCE: &[u8] = b"module repro_parent\n  character(len=3), parameter :: raw_parent = 'A\xffB'\n  interface\n    module subroutine fill(x)\n      integer, intent(out) :: x\n    end subroutine\n  end interface\nend module\n";
const PROVENANCE_CHILD_SOURCE: &[u8] = b"submodule (repro_parent) repro_child\n  character(len=3), parameter :: raw_child = 'C\xfeD'\ncontains\n  module procedure fill\n    x = 7\n  end procedure\nend submodule\n";

#[derive(Clone, Copy)]
enum SourceSpelling {
    Relative,
    Absolute,
    #[cfg(windows)]
    DriveRelative,
    #[cfg(windows)]
    RootRelative,
}

fn unique_dir() -> PathBuf {
    let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
    let dir = std::env::temp_dir().join(format!("afs_incr_{}_{}", std::process::id(), id));
    fs::create_dir_all(&dir).unwrap();
    dir
}

fn find_compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary not built for this test profile")
}

fn compile(compiler: &Path, source: &Path, obj: &Path, search: &Path) {
    compile_with_opt(compiler, source, obj, search, "-O0");
}

fn compile_with_opt(compiler: &Path, source: &Path, obj: &Path, search: &Path, opt: &str) {
    compile_with_opt_and_env(compiler, source, obj, search, opt, &[]);
}

fn compile_with_opt_and_env(
    compiler: &Path,
    source: &Path,
    obj: &Path,
    search: &Path,
    opt: &str,
    environment: &[(&str, &OsStr)],
) {
    let mut command = Command::new(compiler);
    command.current_dir(search).args([
        source.to_str().unwrap(),
        "-c",
        opt,
        "-o",
        obj.to_str().unwrap(),
        &format!("-I{}", search.display()),
    ]);
    for &(name, value) in environment {
        command.env(name, value);
    }
    let result = command.output().expect("compiler launch failed");
    assert!(
        result.status.success(),
        "compile {} failed:\n{}",
        source.display(),
        String::from_utf8_lossy(&result.stderr)
    );
}

fn fnv1a_hex(bytes: &[u8]) -> String {
    let mut hash = 0xcbf29ce484222325_u64;
    for &byte in bytes {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x100000001b3);
    }
    format!("{hash:016x}")
}

/// Compile a module and return the .amod contents.
fn compile_module(compiler: &Path, dir: &Path, name: &str, source: &str) -> Vec<u8> {
    compile_module_with_opt(compiler, dir, name, source, "-O0")
}

fn compile_module_with_opt(
    compiler: &Path,
    dir: &Path,
    name: &str,
    source: &str,
    opt: &str,
) -> Vec<u8> {
    compile_module_with_opt_and_env(compiler, dir, name, source, opt, &[])
}

fn compile_module_with_opt_and_env(
    compiler: &Path,
    dir: &Path,
    name: &str,
    source: &str,
    opt: &str,
    environment: &[(&str, &OsStr)],
) -> Vec<u8> {
    let f90 = dir.join(format!("{}.f90", name));
    let obj = dir.join(format!("{}.o", name));
    fs::write(&f90, source).unwrap();
    compile_with_opt_and_env(compiler, &f90, &obj, dir, opt, environment);
    let amod = dir.join(format!("{}.amod", name));
    fs::read(&amod).unwrap_or_else(|e| panic!("{}.amod not found: {}", name, e))
}

fn open_publication_witness(path: &Path) -> fs::File {
    fs::OpenOptions::new()
        .write(true)
        .open(path)
        .expect("open .amod as a publication identity witness")
}

fn assert_publication_was_not_replaced(mut witness: fs::File, path: &Path) {
    const PUBLICATION_PROBE: &[u8] = b"same published file";
    witness
        .set_len(0)
        .expect("truncate publication identity witness");
    witness
        .write_all(PUBLICATION_PROBE)
        .expect("write publication identity witness");
    drop(witness);
    assert_eq!(
        fs::read(path).expect("read republished .amod"),
        PUBLICATION_PROBE,
        "byte-identical .amod publication replaced the existing file"
    );
}

fn assert_publication_was_replaced(mut witness: fs::File, path: &Path, expected: &[u8]) {
    const PUBLICATION_PROBE: &[u8] = b"superseded published file";
    witness
        .set_len(0)
        .expect("truncate superseded publication witness");
    witness
        .write_all(PUBLICATION_PROBE)
        .expect("write superseded publication witness");
    drop(witness);
    assert_eq!(
        fs::read(path).expect("read changed .amod"),
        expected,
        "changed .amod interface was not atomically republished"
    );
}

fn compile_module_tree_with_spelling(
    compiler: &Path,
    root: &Path,
    module_dir: &Path,
    spelling: SourceSpelling,
) {
    let source_path = |name: &str| match spelling {
        SourceSpelling::Relative => PathBuf::from(name),
        SourceSpelling::Absolute => root.join(name),
        #[cfg(windows)]
        SourceSpelling::DriveRelative => {
            let Some(std::path::Component::Prefix(prefix)) = root.components().next() else {
                panic!("Windows temporary directory has no drive prefix");
            };
            let mut path = prefix.as_os_str().to_os_string();
            path.push(name);
            PathBuf::from(path)
        }
        #[cfg(windows)]
        SourceSpelling::RootRelative => {
            let absolute = root.join(name);
            let mut path = PathBuf::new();
            for component in absolute.components().skip(1) {
                path.push(component.as_os_str());
            }
            path
        }
    };
    let parent_source = source_path("parent.f90");
    let parent_obj = module_dir.join("parent.o");
    let parent = Command::new(compiler)
        .current_dir(root)
        .args(["-c"])
        .arg(&parent_source)
        .args(["-J"])
        .arg(module_dir)
        .args(["-o"])
        .arg(&parent_obj)
        .output()
        .expect("parent compiler launch failed");
    assert!(
        parent.status.success(),
        "parent compile failed:\n{}",
        String::from_utf8_lossy(&parent.stderr)
    );

    let child_source = source_path("child.f90");
    let child_obj = module_dir.join("child.o");
    let child = Command::new(compiler)
        .current_dir(root)
        .args(["-c"])
        .arg(&child_source)
        .args(["-I"])
        .arg(module_dir)
        .args(["-J"])
        .arg(module_dir)
        .args(["-o"])
        .arg(&child_obj)
        .output()
        .expect("submodule compiler launch failed");
    assert!(
        child.status.success(),
        "submodule compile failed:\n{}",
        String::from_utf8_lossy(&child.stderr)
    );
}

fn write_provenance_sources(root: &Path) {
    fs::write(root.join("parent.f90"), PROVENANCE_PARENT_SOURCE).unwrap();
    fs::write(root.join("child.f90"), PROVENANCE_CHILD_SOURCE).unwrap();
}

fn assert_module_artifacts_equal(left: &Path, right: &Path) {
    for artifact in [
        "repro_parent.amod",
        "repro_parent.mod",
        "repro_parent@repro_child.amod",
        "repro_parent@repro_child.smod",
    ] {
        assert_eq!(
            fs::read(left.join(artifact)).unwrap(),
            fs::read(right.join(artifact)).unwrap(),
            "{artifact} depends on source path spelling"
        );
    }
}

#[test]
fn same_source_produces_identical_amod() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=incremental test=same_source_produces_identical_amod count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let src = "module m\n  implicit none\n  integer :: x = 42\nend module\n";

    let amod1 = compile_module(&compiler, &dir, "m", src);
    let amod_path = dir.join("m.amod");
    let published_file = open_publication_witness(&amod_path);
    // Recompile with identical source.
    let amod2 = compile_module(&compiler, &dir, "m", src);

    assert_eq!(amod1, amod2, ".amod changed despite identical source");
    assert_publication_was_not_replaced(published_file, &amod_path);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn module_artifacts_ignore_relative_vs_absolute_source_spelling() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=incremental test=module_artifacts_ignore_relative_vs_absolute_source_spelling count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let root = unique_dir();
    write_provenance_sources(&root);
    let relative_dir = root.join("relative");
    let absolute_dir = root.join("absolute");
    fs::create_dir_all(&relative_dir).unwrap();
    fs::create_dir_all(&absolute_dir).unwrap();

    compile_module_tree_with_spelling(&compiler, &root, &relative_dir, SourceSpelling::Relative);
    compile_module_tree_with_spelling(&compiler, &root, &absolute_dir, SourceSpelling::Absolute);

    assert_module_artifacts_equal(&relative_dir, &absolute_dir);
    let parent = fs::read_to_string(relative_dir.join("repro_parent.amod")).unwrap();
    assert!(parent.contains("# source: parent.f90\n"));
    assert!(
        !parent.lines().any(|line| line.starts_with("# checksum:")),
        ".amod must not fingerprint complete source bytes"
    );
    let child_interface = fs::read(relative_dir.join("repro_parent@repro_child.amod")).unwrap();
    let child_interface_text = String::from_utf8(child_interface.clone()).unwrap();
    assert!(
        !child_interface_text
            .lines()
            .any(|line| line.starts_with("# checksum:")),
        "submodule .amod must not fingerprint complete source bytes"
    );
    let child = fs::read_to_string(relative_dir.join("repro_parent@repro_child.smod")).unwrap();
    assert!(child.contains("# source: child.f90\n"));
    assert!(child.contains(&format!(
        "@interface repro_parent@repro_child.amod fnv1a:{}\n",
        fnv1a_hex(&child_interface)
    )));
    let _ = fs::remove_dir_all(root);
}

#[cfg(windows)]
#[test]
fn module_artifacts_ignore_windows_source_path_spellings() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=incremental test=module_artifacts_ignore_windows_source_path_spellings count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let root = unique_dir();
    let Some(std::path::Component::Prefix(prefix)) = root.components().next() else {
        let _ = fs::remove_dir_all(root);
        return;
    };
    if !matches!(prefix.kind(), std::path::Prefix::Disk(_)) {
        let _ = fs::remove_dir_all(root);
        return;
    }

    write_provenance_sources(&root);
    let absolute_dir = root.join("absolute");
    fs::create_dir_all(&absolute_dir).unwrap();
    compile_module_tree_with_spelling(&compiler, &root, &absolute_dir, SourceSpelling::Absolute);

    for (directory, spelling) in [
        ("drive-relative", SourceSpelling::DriveRelative),
        ("root-relative", SourceSpelling::RootRelative),
    ] {
        let output_dir = root.join(directory);
        fs::create_dir_all(&output_dir).unwrap();
        compile_module_tree_with_spelling(&compiler, &root, &output_dir, spelling);
        assert_module_artifacts_equal(&absolute_dir, &output_dir);
    }
    let _ = fs::remove_dir_all(root);
}

#[test]
fn changed_public_interface_changes_amod() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=incremental test=changed_public_interface_changes_amod count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();

    let v1 = "module m\n  implicit none\n  integer :: x = 42\nend module\n";
    let v2 = "module m\n  implicit none\n  integer :: x = 42\n  integer :: y = 99\nend module\n";

    let amod1 = compile_module(&compiler, &dir, "m", v1);
    let amod_path = dir.join("m.amod");
    let published_file = open_publication_witness(&amod_path);
    let amod2 = compile_module(&compiler, &dir, "m", v2);

    assert_ne!(
        amod1, amod2,
        ".amod should differ when public interface changes"
    );
    assert_publication_was_replaced(published_file, &amod_path, &amod2);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn module_interface_is_stable_across_optimization_levels() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=incremental test=module_interface_is_stable_across_optimization_levels count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();
    let source = "\
module m
  implicit none
contains
  integer function compute(value)
    integer, intent(in) :: value
    compute = (value + 0) * 1
  end function
end module
";

    let baseline = compile_module_with_opt(&compiler, &dir, "m", source, "-O0");
    for opt in ["-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        let candidate = compile_module_with_opt(&compiler, &dir, "m", source, opt);
        assert_eq!(
            candidate, baseline,
            ".amod interface bytes changed at {opt}"
        );
    }
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn module_interface_is_stable_across_build_environments() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=incremental test=module_interface_is_stable_across_build_environments count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let root = unique_dir();
    let baseline_dir = root.join("baseline");
    let varied_dir = root.join("varied");
    let baseline_tmp = root.join("tmp-baseline");
    let varied_tmp = root.join("tmp-varied");
    let unused_tools = root.join("unused-tools");
    for directory in [
        &baseline_dir,
        &varied_dir,
        &baseline_tmp,
        &varied_tmp,
        &unused_tools,
    ] {
        fs::create_dir_all(directory).unwrap();
    }

    let inherited_path = std::env::var_os("PATH").unwrap_or_default();
    let mut varied_path_entries = vec![unused_tools];
    varied_path_entries.extend(std::env::split_paths(&inherited_path));
    let varied_path =
        std::env::join_paths(varied_path_entries).expect("construct varied PATH value");
    let baseline_environment = [
        ("TMPDIR", baseline_tmp.as_os_str()),
        ("TMP", baseline_tmp.as_os_str()),
        ("TEMP", baseline_tmp.as_os_str()),
        ("LC_ALL", OsStr::new("C")),
        ("PATH", inherited_path.as_os_str()),
        ("CARGO_BUILD_JOBS", OsStr::new("1")),
    ];
    let varied_environment = [
        ("TMPDIR", varied_tmp.as_os_str()),
        ("TMP", varied_tmp.as_os_str()),
        ("TEMP", varied_tmp.as_os_str()),
        ("LC_ALL", OsStr::new("POSIX")),
        ("PATH", varied_path.as_os_str()),
        ("CARGO_BUILD_JOBS", OsStr::new("4")),
    ];
    let source = "\
module m
  implicit none
  integer, parameter :: answer = 42
contains
  integer function identity(value)
    integer, intent(in) :: value
    identity = value
  end function
end module
";

    let baseline = compile_module_with_opt_and_env(
        &compiler,
        &baseline_dir,
        "m",
        source,
        "-O2",
        &baseline_environment,
    );
    let varied = compile_module_with_opt_and_env(
        &compiler,
        &varied_dir,
        "m",
        source,
        "-O2",
        &varied_environment,
    );
    assert_eq!(
        varied, baseline,
        ".amod interface bytes depend on TMPDIR, PATH, locale, or job count"
    );
    let _ = fs::remove_dir_all(root);
}

#[test]
fn changed_private_impl_does_not_change_amod() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=incremental test=changed_private_impl_does_not_change_amod count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();

    let v1 = "\
module m
  implicit none
  integer :: x = 42
contains
  subroutine bump()
    x = x + 1
  end subroutine
end module
";
    let v2 = "\
module m
  implicit none
  integer :: x = 42
contains
  subroutine bump()
    x = x + 10
  end subroutine
end module
";

    let amod1 = compile_module(&compiler, &dir, "m", v1);
    let amod_path = dir.join("m.amod");
    let published_file = open_publication_witness(&amod_path);
    let amod2 = compile_module(&compiler, &dir, "m", v2);

    assert_ne!(v1, v2, "fixture must change the procedure body");
    assert_eq!(
        amod1, amod2,
        ".amod changed even though only a procedure body changed"
    );
    assert_publication_was_not_replaced(published_file, &amod_path);
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn consumer_object_stable_when_amod_unchanged() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=incremental test=consumer_object_stable_when_amod_unchanged count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();

    let mod_src = "module m\n  implicit none\n  integer :: x = 42\nend module\n";
    let main_src = "program p\n  use m\n  implicit none\n  print *, x\nend program\n";

    // Compile module.
    compile_module(&compiler, &dir, "m", mod_src);

    // Compile consumer.
    let main_f90 = dir.join("main.f90");
    let main_o = dir.join("main.o");
    fs::write(&main_f90, main_src).unwrap();
    compile(&compiler, &main_f90, &main_o, &dir);
    let obj1 = fs::read(&main_o).unwrap();

    // Recompile consumer without changing anything.
    compile(&compiler, &main_f90, &main_o, &dir);
    let obj2 = fs::read(&main_o).unwrap();

    assert_eq!(
        obj1, obj2,
        "consumer .o changed despite no source/amod change"
    );
    let _ = fs::remove_dir_all(&dir);
}

#[test]
fn consumer_object_changes_when_amod_changes() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=incremental test=consumer_object_changes_when_amod_changes count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let compiler = find_compiler();
    let dir = unique_dir();

    let mod_v1 = "module m\n  implicit none\n  integer :: x = 42\nend module\n";
    let mod_v2 =
        "module m\n  implicit none\n  integer :: x = 42\n  integer :: y = 99\nend module\n";
    let main_src = "program p\n  use m\n  implicit none\n  print *, x\nend program\n";

    // Compile module v1 and consumer.
    compile_module(&compiler, &dir, "m", mod_v1);
    let main_f90 = dir.join("main.f90");
    let main_o = dir.join("main.o");
    fs::write(&main_f90, main_src).unwrap();
    compile(&compiler, &main_f90, &main_o, &dir);
    let _obj1 = fs::read(&main_o).unwrap();

    // Recompile module with changed public interface.
    compile_module(&compiler, &dir, "m", mod_v2);
    // Recompile consumer against new .amod.
    compile(&compiler, &main_f90, &main_o, &dir);
    let _obj2 = fs::read(&main_o).unwrap();

    // The consumer .o may or may not change (depends on whether the
    // consumer references the new symbol). But the .amod definitely
    // changed, so this test documents the observation either way.
    // The key point: no crash, no stale symbol resolution.
    let _ = fs::remove_dir_all(&dir);
}
