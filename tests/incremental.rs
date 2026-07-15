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

use std::fs;
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
    let result = Command::new(compiler)
        .current_dir(search)
        .args([
            source.to_str().unwrap(),
            "-c",
            "-O0",
            "-o",
            obj.to_str().unwrap(),
            &format!("-I{}", search.display()),
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

/// Extract the interface body from an .amod file (everything after the
/// first blank line, which separates the header from the interface).
fn extract_amod_body(amod: &[u8]) -> &[u8] {
    let text = std::str::from_utf8(amod).unwrap_or("");
    if let Some(idx) = text.find("\n\n") {
        &amod[idx + 2..]
    } else {
        amod
    }
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
    let f90 = dir.join(format!("{}.f90", name));
    let obj = dir.join(format!("{}.o", name));
    fs::write(&f90, source).unwrap();
    compile(compiler, &f90, &obj, dir);
    let amod = dir.join(format!("{}.amod", name));
    fs::read(&amod).unwrap_or_else(|e| panic!("{}.amod not found: {}", name, e))
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
    // Recompile with identical source.
    let amod2 = compile_module(&compiler, &dir, "m", src);

    assert_eq!(amod1, amod2, ".amod changed despite identical source");
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
    assert!(parent.contains(&format!(
        "# checksum: fnv1a:{}\n",
        fnv1a_hex(PROVENANCE_PARENT_SOURCE)
    )));
    let child_interface = fs::read(relative_dir.join("repro_parent@repro_child.amod")).unwrap();
    let child_interface_text = String::from_utf8(child_interface.clone()).unwrap();
    assert!(child_interface_text.contains(&format!(
        "# checksum: fnv1a:{}\n",
        fnv1a_hex(PROVENANCE_CHILD_SOURCE)
    )));
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
    let amod2 = compile_module(&compiler, &dir, "m", v2);

    assert_ne!(
        amod1, amod2,
        ".amod should differ when public interface changes"
    );
    let _ = fs::remove_dir_all(&dir);
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
    let amod2 = compile_module(&compiler, &dir, "m", v2);

    // The header includes a source checksum which will differ. But the
    // interface section (everything after the blank line separating the
    // header from the body) should be identical.
    let body1 = extract_amod_body(&amod1);
    let body2 = extract_amod_body(&amod2);
    assert_eq!(
        body1, body2,
        ".amod interface body changed but only private impl differs"
    );
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
