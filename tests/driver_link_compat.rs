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
    let smod_text = std::fs::read_to_string(&smod).expect("missing .smod");
    assert!(
        smod_text.starts_with("#!smod 2\n"),
        "smod should use the checksum-bound format"
    );
    assert!(
        smod_text.contains("@parent smod_parent"),
        "smod should record its parent"
    );
    assert!(
        smod_text.contains("@interface smod_parent@smod_child.amod"),
        "smod should identify its semantic interface"
    );
    assert!(
        smod_text.contains(" fnv1a:"),
        "smod should bind its semantic interface by checksum"
    );
    let interface = mod_dir.join("smod_parent@smod_child.amod");
    assert!(interface.exists(), "expected submodule semantic interface");
    let interface_text = std::fs::read_to_string(interface).expect("missing submodule interface");
    assert!(
        interface_text.contains("# module: smod_child")
            && interface_text.contains("# ancestor-module: smod_parent"),
        "submodule interface should retain its semantic identity"
    );
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn assembly_only_publishes_module_and_submodule_interfaces() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_link_compat test=assembly_only_publishes_module_and_submodule_interfaces count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let dir = unique_dir("assembly_module_interfaces");
    let parent = write_program_in(
        &dir,
        "parent.f90",
        "module asm_parent\n  interface\n    module subroutine fill(x)\n      integer, intent(out) :: x\n    end subroutine\n  end interface\nend module\n",
    );
    let child = write_program_in(
        &dir,
        "child.f90",
        "submodule (asm_parent) asm_child\ncontains\n  module procedure fill\n    x = 7\n  end procedure\nend submodule\n",
    );
    let mod_dir = dir.join("mods");
    std::fs::create_dir_all(&mod_dir).expect("cannot create module dir");

    let parent_mod = mod_dir.join("asm_parent.mod");
    std::fs::write(&parent_mod, "stale module alias\n").expect("cannot seed stale .mod");
    let parent_asm = dir.join("parent.s");
    let parent_result = Command::new(compiler("armfortas"))
        .args([
            "-S",
            parent.to_str().unwrap(),
            "-J",
            mod_dir.to_str().unwrap(),
            "-o",
            parent_asm.to_str().unwrap(),
        ])
        .output()
        .expect("parent assembly compile spawn failed");
    assert!(
        parent_result.status.success(),
        "parent assembly compile failed: {}",
        String::from_utf8_lossy(&parent_result.stderr)
    );
    let parent_amod = mod_dir.join("asm_parent.amod");
    let parent_interface =
        std::fs::read_to_string(&parent_amod).expect("assembly-only build omitted parent .amod");
    assert_eq!(
        parent_interface,
        std::fs::read_to_string(&parent_mod).expect("assembly-only build omitted parent .mod"),
        "assembly-only build should replace the stale .mod with the canonical interface"
    );
    assert!(
        parent_asm.exists(),
        "assembly-only parent build omitted its primary output"
    );

    let child_smod = mod_dir.join("asm_parent@asm_child.smod");
    std::fs::write(&child_smod, "stale submodule alias\n").expect("cannot seed stale .smod");
    let child_asm = dir.join("child.s");
    let child_result = Command::new(compiler("armfortas"))
        .args([
            "-S",
            child.to_str().unwrap(),
            "-I",
            mod_dir.to_str().unwrap(),
            "-J",
            mod_dir.to_str().unwrap(),
            "-o",
            child_asm.to_str().unwrap(),
        ])
        .output()
        .expect("submodule assembly compile spawn failed");
    assert!(
        child_result.status.success(),
        "submodule assembly compile failed: {}",
        String::from_utf8_lossy(&child_result.stderr)
    );
    let child_interface = mod_dir.join("asm_parent@asm_child.amod");
    assert!(
        std::fs::read_to_string(&child_interface)
            .expect("assembly-only build omitted submodule interface")
            .contains("# ancestor-module: asm_parent"),
        "submodule interface should retain its parent identity"
    );
    let smod_text =
        std::fs::read_to_string(&child_smod).expect("assembly-only build omitted .smod alias");
    assert!(
        smod_text.starts_with("#!smod 2\n")
            && smod_text.contains("@interface asm_parent@asm_child.amod"),
        "assembly-only build should replace the stale .smod: {smod_text}"
    );
    assert!(
        child_asm.exists(),
        "assembly-only submodule build omitted its primary output"
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
fn compile_and_link_publishes_dependency_file_after_success() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_link_compat test=compile_and_link_publishes_dependency_file_after_success count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let dir = unique_dir("linked_depfile");
    let include = write_program_in(&dir, "answer.inc", "integer, parameter :: answer = 42\n");
    let src = write_program_in(
        &dir,
        "main.F90",
        "program p\n#include \"answer.inc\"\n  print *, answer\nend program\n",
    );
    for optimization in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
        let output = dir.join(format!("app-{}", &optimization[2..]));
        let depfile = output.with_extension("d");
        std::fs::write(&depfile, "stale dependency output\n")
            .expect("cannot seed stale dependency file");

        let result = Command::new(compiler("armfortas"))
            .arg(optimization)
            .args(["-MD", "-MP", "-MF"])
            .arg(&depfile)
            .arg(&src)
            .arg("-o")
            .arg(&output)
            .output()
            .expect("compile-and-link spawn failed");
        assert!(
            result.status.success(),
            "compile-and-link failed at {optimization}: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(output.is_file(), "linked executable was not published");

        let deps = std::fs::read_to_string(&depfile).expect("missing linked dependency file");
        assert!(
            deps.starts_with(&format!("{}: ", output.display())),
            "default dependency target must be the final executable at {optimization}:\n{deps}"
        );
        for prerequisite in [&src, &include] {
            assert!(
                deps.contains(prerequisite.to_str().unwrap()),
                "linked dependency file omitted {} at {optimization}:\n{deps}",
                prerequisite.display()
            );
        }
        assert!(
            deps.contains(&format!("\n{}:\n", include.display())),
            "-MP omitted the include phony rule at {optimization}:\n{deps}"
        );
        assert!(
            !deps.contains("stale dependency output"),
            "stale dependency text survived at {optimization}"
        );

        let run = Command::new(&output)
            .output()
            .expect("linked dependency witness failed to run");
        assert!(
            run.status.success() && String::from_utf8_lossy(&run.stdout).contains("42"),
            "linked dependency witness produced the wrong result at {optimization}: status={:?} stdout={} stderr={}",
            run.status,
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn multi_source_link_publishes_one_combined_dependency_file() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_link_compat test=multi_source_link_publishes_one_combined_dependency_file count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let dir = unique_dir("multi_link_depfile");
    let include = write_program_in(&dir, "answer.inc", "integer, parameter :: value = 42\n");
    let provider = write_program_in(
        &dir,
        "provider.F90",
        "module values\n#include \"answer.inc\"\ncontains\n  integer function answer()\n    answer = value\n  end function\nend module\n",
    );
    let main = write_program_in(
        &dir,
        "main.f90",
        "program p\n  use values\n  print *, answer()\nend program\n",
    );

    for explicit_depfile in [false, true] {
        let stem = if explicit_depfile {
            "explicit"
        } else {
            "default"
        };
        let output = dir.join(stem);
        let depfile = if explicit_depfile {
            dir.join("deps").join("combined.d")
        } else {
            output.with_extension("d")
        };
        if let Some(parent) = depfile.parent() {
            std::fs::create_dir_all(parent).expect("cannot create depfile parent");
        }
        std::fs::write(&depfile, "stale dependency output\n")
            .expect("cannot seed stale dependency file");

        let mut command = Command::new(compiler("armfortas"));
        command.args(["-MD", "-MP"]);
        if explicit_depfile {
            command
                .arg("-MF")
                .arg(&depfile)
                .arg("-MT")
                .arg("combined_target");
        }
        let result = command
            // Deliberately put the consumer first. Compilation must reorder
            // for the module edge, while dependency text stays in CLI order.
            .arg(&main)
            .arg(&provider)
            .arg("-o")
            .arg(&output)
            .output()
            .expect("multi-source compile-and-link spawn failed");
        assert!(
            result.status.success(),
            "multi-source compile-and-link failed: {}",
            String::from_utf8_lossy(&result.stderr)
        );

        let deps = std::fs::read_to_string(&depfile).expect("missing combined dependency file");
        assert_eq!(
            deps,
            format!(
                "{}: {} {} {}\n\n{}:\n",
                if explicit_depfile {
                    "combined_target".to_string()
                } else {
                    output.display().to_string()
                },
                main.display(),
                provider.display(),
                include.display(),
                include.display()
            ),
            "outer dependency ownership must use final paths and CLI source order"
        );
        assert!(
            !deps.contains("afs_multi_") && !deps.contains("source_0.o"),
            "temporary child paths escaped into the dependency file:\n{deps}"
        );

        let run = Command::new(&output)
            .output()
            .expect("multi-source dependency witness failed to run");
        assert!(
            run.status.success() && String::from_utf8_lossy(&run.stdout).contains("42"),
            "multi-source dependency witness produced the wrong result: status={:?} stdout={} stderr={}",
            run.status,
            String::from_utf8_lossy(&run.stdout),
            String::from_utf8_lossy(&run.stderr)
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn failed_link_removes_stale_dependency_file() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_link_compat test=failed_link_removes_stale_dependency_file count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let dir = unique_dir("failed_link_depfile");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  call missing_external()\nend program\n",
    );
    let output = dir.join("app");
    let depfile = dir.join("app.d");
    std::fs::write(&depfile, "stale dependency output\n")
        .expect("cannot seed stale dependency file");

    let result = Command::new(compiler("armfortas"))
        .args(["-MD", "-MF"])
        .arg(&depfile)
        .arg(&src)
        .arg("-o")
        .arg(&output)
        .output()
        .expect("failing compile-and-link spawn failed");
    assert!(!result.status.success(), "undefined symbol link succeeded");
    assert!(
        !depfile.exists(),
        "failed link left a stale dependency file"
    );
    assert!(!output.exists(), "failed link left a stale executable");

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
#[cfg(unix)]
fn failed_dependency_publication_removes_fresh_executable() {
    use std::os::unix::fs::PermissionsExt;

    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_link_compat test=failed_dependency_publication_removes_fresh_executable count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let dir = unique_dir("failed_depfile_publication");
    let src = write_program_in(&dir, "main.f90", "program p\n  print *, 42\nend program\n");
    let output = dir.join("app");
    let blocked_parent = dir.join("not-a-directory");
    let depfile = blocked_parent.join("app.d");
    std::fs::create_dir(&blocked_parent).expect("cannot create initial depfile directory");
    let linker = write_program_in(
        &dir,
        "linker.sh",
        "#!/bin/sh\n\
         output=\n\
         while [ \"$#\" -gt 0 ]; do\n\
           if [ \"$1\" = \"-o\" ]; then\n\
             shift\n\
             output=$1\n\
           fi\n\
           shift\n\
         done\n\
         [ -n \"$output\" ] || exit 90\n\
         printf 'fresh linked output\\n' > \"$output\" || exit 91\n\
         rmdir \"$DEPFILE_PARENT_TO_BLOCK\" || exit 92\n\
         printf 'preserve this blocker\\n' > \"$DEPFILE_PARENT_TO_BLOCK\" || exit 93\n",
    );
    std::fs::set_permissions(&linker, std::fs::Permissions::from_mode(0o755))
        .expect("cannot make fake linker executable");

    let result = Command::new(compiler("armfortas"))
        .env("AFS_LD_PATH", &linker)
        .env("DEPFILE_PARENT_TO_BLOCK", &blocked_parent)
        .args(["-MD", "-MF"])
        .arg(&depfile)
        .arg(&src)
        .arg("-o")
        .arg(&output)
        .output()
        .expect("depfile publication failure probe failed to spawn");
    assert!(
        !result.status.success(),
        "compile-and-link succeeded without publishing its requested depfile"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("cannot create depfile directory"),
        "missing depfile publication diagnostic: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert!(
        !output.exists(),
        "failed dependency publication left a fresh executable"
    );
    assert_eq!(
        std::fs::read_to_string(&blocked_parent).expect("depfile parent blocker disappeared"),
        "preserve this blocker\n",
        "dependency failure mutated its blocking destination"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn linked_dependency_file_cannot_alias_the_executable() {
    let dir = unique_dir("linked_depfile_alias");
    let src = write_program_in(&dir, "main.f90", "program p\n  print *, 42\nend program\n");
    let output = dir.join("app");
    let original = b"preexisting executable";
    std::fs::write(&output, original).expect("cannot seed existing output");

    let result = Command::new(compiler("armfortas"))
        .args(["-MD", "-MF"])
        .arg(&output)
        .arg(&src)
        .arg("-o")
        .arg(&output)
        .output()
        .expect("depfile/output alias probe failed to spawn");
    assert!(
        !result.status.success(),
        "dependency file aliasing the executable was accepted"
    );
    assert!(
        String::from_utf8_lossy(&result.stderr).contains("conflicts with output"),
        "missing depfile/output conflict diagnostic: {}",
        String::from_utf8_lossy(&result.stderr)
    );
    assert_eq!(
        std::fs::read(&output).expect("preexisting output disappeared"),
        original,
        "destination conflict mutated the preexisting output"
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn linked_dependency_file_cannot_alias_compiler_inputs() {
    let dir = unique_dir("linked_depfile_input_alias");
    let include_text = "integer, parameter :: answer = 42\n";
    let source_text = "program p\n#include \"answer.inc\"\n  print *, answer\nend program\n";
    let include = write_program_in(&dir, "answer.inc", include_text);
    let src = write_program_in(&dir, "main.F90", source_text);

    for (name, depfile) in [("source", &src), ("include", &include)] {
        let output = dir.join(format!("app-{name}"));
        let result = Command::new(compiler("armfortas"))
            .args(["-MD", "-MF"])
            .arg(depfile)
            .arg(&src)
            .arg("-o")
            .arg(&output)
            .output()
            .expect("depfile/input alias probe failed to spawn");
        assert!(
            !result.status.success(),
            "dependency file aliasing the {name} input was accepted"
        );
        assert!(
            String::from_utf8_lossy(&result.stderr).contains("conflicts with compiler input"),
            "missing depfile/{name} conflict diagnostic: {}",
            String::from_utf8_lossy(&result.stderr)
        );
        assert!(
            !output.exists(),
            "depfile/{name} conflict published an executable"
        );
        assert_eq!(
            std::fs::read_to_string(&src).expect("source input disappeared"),
            source_text,
            "depfile/{name} conflict mutated the source input"
        );
        assert_eq!(
            std::fs::read_to_string(&include).expect("include input disappeared"),
            include_text,
            "depfile/{name} conflict mutated the include input"
        );
    }

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn depfile_distinguishes_mt_from_mq_and_quotes_dollar_paths() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_link_compat test=depfile_distinguishes_mt_from_mq_and_quotes_dollar_paths count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let dir = unique_dir("depfile_dollar_quoting");
    let src = write_program_in(
        &dir,
        "source$unit.f90",
        "program dep_quote\n  print *, 42\nend program dep_quote\n",
    );
    let obj = dir.join("object$unit.o");
    let default_depfile = dir.join("default.d");
    std::fs::write(&default_depfile, "stale dependency output\n")
        .expect("cannot seed stale dependency file");

    let default_result = Command::new(compiler("armfortas"))
        .args(["-MD", "-MF"])
        .arg(&default_depfile)
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("default-target compile spawn failed");
    assert!(
        default_result.status.success(),
        "default-target compile failed: {}",
        String::from_utf8_lossy(&default_result.stderr)
    );
    let default_deps =
        std::fs::read_to_string(&default_depfile).expect("missing default dependency file");
    assert_eq!(
        default_deps,
        format!(
            "{}: {}\n",
            obj.to_string_lossy().replace('$', "$$"),
            src.to_string_lossy().replace('$', "$$")
        ),
        "default targets and prerequisite paths must quote dollars for GNU make"
    );

    let explicit_depfile = dir.join("explicit.d");
    let raw_target = "raw target$mt";
    let quoted_target = "quoted target$mq";
    let explicit_result = Command::new(compiler("armfortas"))
        .args(["-MD", "-MF"])
        .arg(&explicit_depfile)
        .arg("-MT")
        .arg(raw_target)
        .arg(format!("-MQ{quoted_target}"))
        .arg("-c")
        .arg(&src)
        .arg("-o")
        .arg(&obj)
        .output()
        .expect("explicit-target compile spawn failed");
    assert!(
        explicit_result.status.success(),
        "explicit-target compile failed: {}",
        String::from_utf8_lossy(&explicit_result.stderr)
    );
    let explicit_deps =
        std::fs::read_to_string(&explicit_depfile).expect("missing explicit dependency file");
    assert_eq!(
        explicit_deps,
        format!(
            "{raw_target} quoted\\ target$$mq: {}\n",
            src.to_string_lossy().replace('$', "$$")
        ),
        "-MT must remain verbatim while -MQ receives make quoting"
    );

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
fn ordinary_fortran_include_compiles_runs_and_tracks_transitive_dependencies() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=driver_link_compat test=ordinary_fortran_include_compiles_runs_and_tracks_transitive_dependencies count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let dir = unique_dir("ordinary_fortran_include");
    let inner = write_program_in(
        &dir,
        "inner.inc",
        "  integer, parameter :: included_answer = 42\n",
    );
    let outer = write_program_in(&dir, "outer.inc", "include \"inner.inc\"\n");
    let source = write_program_in(
        &dir,
        "main.f90",
        "program p\n  include 'outer.inc' ! standard Fortran include\n  print *, included_answer\nend program\n",
    );
    let output = dir.join("free-include");
    let depfile = dir.join("free-include.d");

    let compile = Command::new(compiler("armfortas"))
        .args(["-O2", "-MMD", "-MP", "-MF"])
        .arg(&depfile)
        .arg(&source)
        .arg("-o")
        .arg(&output)
        .output()
        .expect("ordinary INCLUDE compile spawn failed");
    assert!(
        compile.status.success(),
        "ordinary INCLUDE compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&output)
        .output()
        .expect("ordinary INCLUDE executable failed to run");
    assert!(
        run.status.success() && String::from_utf8_lossy(&run.stdout).contains("42"),
        "ordinary INCLUDE executable produced the wrong result: status={:?} stdout={} stderr={}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let dependencies = std::fs::read_to_string(&depfile).expect("missing dependency file");
    for prerequisite in [&source, &outer, &inner] {
        assert!(
            dependencies.contains(prerequisite.to_str().unwrap()),
            "dependency file omitted {}:\n{dependencies}",
            prerequisite.display()
        );
    }
    for include in [&outer, &inner] {
        assert!(
            dependencies.contains(&format!("{}:\n", include.display())),
            "-MP omitted the include phony rule for {}:\n{dependencies}",
            include.display()
        );
    }

    let fixed_include = write_program_in(
        &dir,
        "fixed.inc",
        "      INTEGER, PARAMETER :: FIXED_ANSWER = 7\n",
    );
    let fixed_source = write_program_in(
        &dir,
        "main.f",
        "      PROGRAM P\n      I N C L U D E 'fixed.inc'\n      PRINT *, FIXED_ANSWER\n      END\n",
    );
    let fixed_output = dir.join("fixed-include");
    let fixed_compile = Command::new(compiler("armfortas"))
        .args(["-O2"])
        .arg(&fixed_source)
        .arg("-o")
        .arg(&fixed_output)
        .output()
        .expect("fixed-form INCLUDE compile spawn failed");
    assert!(
        fixed_compile.status.success(),
        "fixed-form INCLUDE compile failed: {}",
        String::from_utf8_lossy(&fixed_compile.stderr)
    );
    let fixed_run = Command::new(&fixed_output)
        .output()
        .expect("fixed-form INCLUDE executable failed to run");
    assert!(
        fixed_run.status.success() && String::from_utf8_lossy(&fixed_run.stdout).contains('7'),
        "fixed-form INCLUDE executable produced the wrong result: status={:?} stdout={} stderr={}",
        fixed_run.status,
        String::from_utf8_lossy(&fixed_run.stdout),
        String::from_utf8_lossy(&fixed_run.stderr)
    );
    assert!(fixed_include.is_file(), "fixed include fixture disappeared");

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
