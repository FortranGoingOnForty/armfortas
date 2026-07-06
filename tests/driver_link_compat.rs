use std::path::{Path, PathBuf};
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
    panic!(
        "compiler binary '{}' not built - run `cargo build --bins` first",
        name
    );
}

fn unique_path(stem: &str, ext: &str) -> PathBuf {
    let pid = std::process::id();
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    std::env::temp_dir().join(format!("afs_driver_{}_{}_{}.{}", stem, pid, id, ext))
}

fn unique_dir(stem: &str) -> PathBuf {
    let dir = unique_path(stem, "dir");
    std::fs::create_dir_all(&dir).expect("cannot create temp dir");
    dir
}

fn write_program_in(dir: &Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("cannot write test source");
    path
}

#[test]
fn darwin_loader_at_paths_are_not_response_files() {
    let result = Command::new(compiler("armfortas"))
        .args([
            "--help",
            "@rpath/libprimaf.dylib",
            "@loader_path/libdep.dylib",
            "@executable_path/libdep.dylib",
        ])
        .output()
        .expect("spawn failed");
    assert!(
        result.status.success(),
        "Darwin loader @paths should not be opened as response files: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        String::from_utf8_lossy(&result.stdout).contains("USAGE"),
        "expected --help output"
    );
}

#[test]
fn dash_j_writes_amod_and_mod_alias() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_link_compat test=dash_j_writes_amod_and_mod_alias count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let dir = unique_dir("dash_j_mod_alias");
    let src = write_program_in(
        &dir,
        "m.f90",
        "module m\ncontains\n  integer function answer()\n    answer = 42\n  end function\nend module\n",
    );
    let amod_dir = dir.join("mods");
    std::fs::create_dir_all(&amod_dir).expect("cannot create module dir");
    let out = dir.join("m.o");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            src.to_str().unwrap(),
            "-J",
            amod_dir.to_str().unwrap(),
            "-o",
            out.to_str().unwrap(),
        ])
        .output()
        .expect("compile spawn failed");
    assert!(
        result.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let amod = amod_dir.join("m.amod");
    let mod_alias = amod_dir.join("m.mod");
    assert!(amod.exists(), "expected .amod output");
    assert!(mod_alias.exists(), "expected .mod compatibility alias");
    assert_eq!(
        std::fs::read_to_string(&amod).expect("missing .amod"),
        std::fs::read_to_string(&mod_alias).expect("missing .mod")
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn gnu_depfile_flags_write_make_dependency_file() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_link_compat test=gnu_depfile_flags_write_make_dependency_file count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let dir = unique_dir("gnu_depfile_flags");
    let src = write_program_in(
        &dir,
        "m.f90",
        "module m\ncontains\n  integer function answer()\n    answer = 42\n  end function\nend module\n",
    );
    let amod_dir = dir.join("mods");
    std::fs::create_dir_all(&amod_dir).expect("cannot create module dir");
    let obj = dir.join("m.o");
    let depfile = dir.join("m.d");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-fPIC",
            "-MD",
            "-MF",
            depfile.to_str().unwrap(),
            "-MT",
            "custom_target",
            "-c",
            src.to_str().unwrap(),
            "-J",
            amod_dir.to_str().unwrap(),
            "-o",
            obj.to_str().unwrap(),
        ])
        .output()
        .expect("compile spawn failed");
    assert!(
        result.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    let deps = std::fs::read_to_string(&depfile).expect("missing dependency file");
    assert!(
        deps.contains("custom_target:"),
        "dependency file should use -MT target, got: {deps}"
    );
    assert!(
        deps.contains(src.to_str().unwrap()),
        "dependency file should mention source, got: {deps}"
    );
    assert!(amod_dir.join("m.amod").exists(), "expected .amod output");
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn dynamiclib_driver_spelling_forwards_darwin_linker_flags() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_link_compat test=dynamiclib_driver_spelling_forwards_darwin_linker_flags count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    if armfortas::testing::native_macho_toolchain_support().is_err() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_link_compat test=dynamiclib_driver_spelling_forwards_darwin_linker_flags count=1 reason=\"Mach-O dylib flow only\""
        );
        return;
    }

    let dir = unique_dir("dynamiclib_flags");
    let src = write_program_in(
        &dir,
        "m.f90",
        "module m\ncontains\n  integer function answer()\n    answer = 42\n  end function\nend module\n",
    );
    let dylib = dir.join("libm.dylib");
    let result = Command::new(compiler("armfortas"))
        .args([
            "-dynamiclib",
            "-Wl,-headerpad_max_install_names",
            "-install_name",
            "@rpath/libm.dylib",
            src.to_str().unwrap(),
            "-o",
            dylib.to_str().unwrap(),
        ])
        .output()
        .expect("dynamiclib compile spawn failed");
    assert!(
        result.status.success(),
        "dynamiclib compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(dylib.exists(), "dynamiclib output should exist");
    assert!(dir.join("m.amod").exists(), "dynamiclib compile should emit .amod");
    assert!(dir.join("m.mod").exists(), "dynamiclib compile should emit .mod alias");
    let _ = std::fs::remove_dir_all(&dir);
}
