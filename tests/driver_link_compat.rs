use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

fn compiler(name: &str) -> PathBuf {
    armfortas::testing::built_binary(name)
        .unwrap_or_else(|| panic!("compiler binary '{name}' not built for this test profile"))
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
fn dash_j_writes_smod_alias_for_submodule() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_link_compat test=dash_j_writes_smod_alias_for_submodule count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let dir = unique_dir("dash_j_smod_alias");
    let parent = write_program_in(
        &dir,
        "parent.f90",
        "module smod_parent\n  interface\n    module subroutine fill(x)\n      integer, intent(out) :: x\n    end subroutine\n  end interface\nend module\n",
    );
    let child = write_program_in(
        &dir,
        "child.f90",
        "submodule (smod_parent) smod_child\ncontains\n  module procedure fill\n    x = 7\n  end procedure\nend submodule\n",
    );
    let mod_dir = dir.join("mods");
    std::fs::create_dir_all(&mod_dir).expect("cannot create module dir");

    let parent_obj = dir.join("parent.o");
    let parent_result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            parent.to_str().unwrap(),
            "-J",
            mod_dir.to_str().unwrap(),
            "-o",
            parent_obj.to_str().unwrap(),
        ])
        .output()
        .expect("parent compile spawn failed");
    assert!(
        parent_result.status.success(),
        "parent compile failed: {}",
        String::from_utf8_lossy(&parent_result.stderr)
    );

    let child_obj = dir.join("child.o");
    let child_result = Command::new(compiler("armfortas"))
        .args([
            "-c",
            child.to_str().unwrap(),
            "-I",
            mod_dir.to_str().unwrap(),
            "-J",
            mod_dir.to_str().unwrap(),
            "-o",
            child_obj.to_str().unwrap(),
        ])
        .output()
        .expect("submodule compile spawn failed");
    assert!(
        child_result.status.success(),
        "submodule compile failed: {}",
        String::from_utf8_lossy(&child_result.stderr)
    );

    let smod = mod_dir.join("smod_parent@smod_child.smod");
    assert!(smod.exists(), "expected CMake-compatible .smod output");
    assert!(
        std::fs::read_to_string(&smod)
            .expect("missing .smod")
            .contains("@parent smod_parent"),
        "smod should record its parent"
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
fn depfile_tracks_transitive_preprocessor_includes() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_link_compat test=depfile_tracks_transitive_preprocessor_includes count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let dir = unique_dir("transitive_depfile_includes");
    let src = write_program_in(
        &dir,
        "main.F90",
        "subroutine value()\n#include \"outer.inc\"\n  print *, answer\nend subroutine\n",
    );
    let outer = write_program_in(&dir, "outer.inc", "#include \"inner.inc\"\n");
    let inner = write_program_in(&dir, "inner.inc", "  integer, parameter :: answer = 42\n");
    let obj = dir.join("main.o");
    let depfile = dir.join("main.d");

    let result = Command::new(compiler("armfortas"))
        .args(["-MMD", "-MP", "-MF"])
        .arg(&depfile)
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("compile spawn failed");
    assert!(
        result.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&result.stderr)
    );

    let deps = std::fs::read_to_string(&depfile).expect("missing dependency file");
    for prerequisite in [&src, &outer, &inner] {
        assert!(
            deps.contains(prerequisite.to_str().unwrap()),
            "dependency file omitted {}:\n{deps}",
            prerequisite.display()
        );
    }
    assert!(
        deps.contains(&format!("{}:\n", outer.display())),
        "-MP omitted the outer include phony rule:\n{deps}"
    );
    assert!(
        deps.contains(&format!("{}:\n", inner.display())),
        "-MP omitted the inner include phony rule:\n{deps}"
    );
    assert!(
        !deps.contains(&format!("\n{}:\n", src.display())),
        "-MP must not emit a phony rule for the primary source:\n{deps}"
    );

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
    assert!(
        dir.join("m.amod").exists(),
        "dynamiclib compile should emit .amod"
    );
    assert!(
        dir.join("m.mod").exists(),
        "dynamiclib compile should emit .mod alias"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn verbose_link_only_darwin_line_exposes_runtime_for_cmake() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_link_compat test=verbose_link_only_darwin_line_exposes_runtime_for_cmake count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    if armfortas::testing::native_macho_toolchain_support().is_err() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_link_compat test=verbose_link_only_darwin_line_exposes_runtime_for_cmake count=1 reason=\"Mach-O link flow only\""
        );
        return;
    }

    let dir = unique_dir("verbose_link_runtime");
    let src = write_program_in(&dir, "p.f90", "program p\n  print *, 'ok'\nend program\n");
    let obj = dir.join("p.o");
    let compile = Command::new(compiler("armfortas"))
        .args(["-c", src.to_str().unwrap(), "-o", obj.to_str().unwrap()])
        .output()
        .expect("object compile spawn failed");
    assert!(
        compile.status.success(),
        "object compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let exe = dir.join("p");
    let link = Command::new(compiler("armfortas"))
        .env_remove("AFS_LD")
        .env_remove("AFS_LD_PATH")
        .args([
            "-v",
            "-Wl,-v",
            obj.to_str().unwrap(),
            "-o",
            exe.to_str().unwrap(),
        ])
        .output()
        .expect("link spawn failed");
    assert!(
        link.status.success(),
        "link failed: {}",
        String::from_utf8_lossy(&link.stderr)
    );
    let stderr = String::from_utf8_lossy(&link.stderr);
    assert!(
        stderr
            .lines()
            .any(|line| line.trim_start().starts_with("ld ")
                && line.contains("libarmfortas_rt.a")),
        "verbose link output should expose an ld command line with the runtime archive for CMake implicit-link parsing:\n{}",
        stderr
    );
    let _ = std::fs::remove_dir_all(&dir);
}
