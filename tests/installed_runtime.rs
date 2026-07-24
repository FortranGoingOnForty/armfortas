use std::fs;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};
use std::sync::atomic::{AtomicU64, Ordering};

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

struct TestDir(PathBuf);

impl TestDir {
    fn new(case: &str) -> Self {
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let path = std::env::temp_dir().join(format!(
            "armfortas-installed-runtime-{}-{case}-{id}",
            std::process::id()
        ));
        fs::create_dir_all(&path).expect("create installed-runtime test directory");
        Self(path)
    }
}

impl AsRef<Path> for TestDir {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for TestDir {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.0);
    }
}

fn assert_success(output: &Output, context: &str) {
    assert!(
        output.status.success(),
        "{context} failed with {:?}\nstdout:\n{}\nstderr:\n{}",
        output.status.code(),
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

#[test]
fn isolated_compiler_binaries_carry_their_runtime_archive() {
    let root = TestDir::new("clean-prefix");
    let prefix_bin = root.as_ref().join("install/bin");
    let outside = root.as_ref().join("outside");
    let runtime_tmp = root.as_ref().join("runtime-tmp");
    let alternate_runtime_tmp = root.as_ref().join("runtime-tmp-alternate");
    fs::create_dir_all(&prefix_bin).expect("create clean install prefix");
    fs::create_dir_all(&outside).expect("create outside-checkout directory");
    fs::create_dir_all(&runtime_tmp).expect("create private runtime temp directory");
    fs::create_dir_all(&alternate_runtime_tmp)
        .expect("create alternate private runtime temp directory");

    let source = outside.join("hello.f90");
    fs::write(
        &source,
        b"program hello\n  print *, \"installed runtime\"\nend program hello\n",
    )
    .expect("write installed-runtime witness");

    for compiler_name in ["armfortas", "afs"] {
        let built = armfortas::testing::built_binary(compiler_name)
            .unwrap_or_else(|| panic!("{compiler_name} binary was not built for this test"));
        let installed = prefix_bin.join(compiler_name);
        fs::copy(&built, &installed)
            .unwrap_or_else(|err| panic!("copy {} into clean prefix: {err}", built.display()));

        let executable = outside.join(format!("hello-{compiler_name}"));
        let compile = Command::new(&installed)
            .current_dir(&outside)
            .env_remove("AFS_RUNTIME_PATH")
            .env_remove("CARGO_TARGET_DIR")
            .env("TMPDIR", &runtime_tmp)
            .arg(&source)
            .arg("-o")
            .arg(&executable)
            .output()
            .unwrap_or_else(|err| panic!("launch copied {compiler_name}: {err}"));
        assert_success(&compile, &format!("copied {compiler_name} compile"));

        let run = Command::new(&executable)
            .output()
            .unwrap_or_else(|err| panic!("run output from copied {compiler_name}: {err}"));
        assert_success(&run, &format!("copied {compiler_name} output"));
        assert_eq!(run.stdout, b" installed runtime\n");
        assert!(
            run.stderr.is_empty(),
            "{compiler_name} wrote unexpected stderr"
        );
    }

    if cfg!(target_os = "linux") {
        let installed = prefix_bin.join("armfortas");
        let repeated = outside.join("hello-armfortas-repeated");
        let compile = Command::new(&installed)
            .current_dir(&outside)
            .env_remove("AFS_RUNTIME_PATH")
            .env_remove("CARGO_TARGET_DIR")
            .env("TMPDIR", &alternate_runtime_tmp)
            .arg(&source)
            .arg("-o")
            .arg(&repeated)
            .output()
            .expect("repeat copied armfortas compile under a different TMPDIR");
        assert_success(&compile, "repeated copied armfortas compile");
        assert_eq!(
            fs::read(outside.join("hello-armfortas")).unwrap(),
            fs::read(&repeated).unwrap(),
            "materialized runtime path made ELF output nondeterministic"
        );
    }

    assert!(
        fs::read_dir(&runtime_tmp)
            .expect("inspect private runtime temp directory")
            .next()
            .is_none(),
        "materialized runtime archive was not cleaned up"
    );
    assert!(
        fs::read_dir(&alternate_runtime_tmp)
            .expect("inspect alternate runtime temp directory")
            .next()
            .is_none(),
        "alternate materialized runtime archive was not cleaned up"
    );
    assert!(
        !prefix_bin.join("libarmfortas_rt.a").exists(),
        "test prefix must not provide an external runtime archive"
    );
}

#[test]
fn installed_compiler_ignores_unrelated_cargo_runtime_trees() {
    let root = TestDir::new("unrelated-workspace");
    let prefix_bin = root.as_ref().join("install/bin");
    let runtime_tmp = root.as_ref().join("runtime-tmp");
    fs::create_dir_all(&prefix_bin).expect("create clean install prefix");
    fs::create_dir_all(&runtime_tmp).expect("create private runtime temp directory");

    let built = armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary was not built for this test");
    let installed = prefix_bin.join("armfortas");
    fs::copy(&built, &installed)
        .unwrap_or_else(|err| panic!("copy {} into clean prefix: {err}", built.display()));

    let mut deterministic_o2 = None;
    for (case, seed_unrelated_archive, seed_stale_output) in [
        ("coincidental-archive", true, false),
        ("cargo-tripwire", false, true),
    ] {
        let workspace = root.as_ref().join(case);
        let runtime_dir = workspace.join("runtime");
        fs::create_dir_all(&runtime_dir).expect("create unrelated runtime directory");
        fs::write(
            workspace.join("Cargo.toml"),
            b"[workspace]\nmembers = [\"runtime\"]\nresolver = \"2\"\n",
        )
        .expect("write unrelated workspace manifest");
        fs::write(
            runtime_dir.join("Cargo.toml"),
            b"[package]\nname = \"unrelated-runtime\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .expect("write unrelated runtime manifest");

        let unrelated_archives = [
            workspace.join("target/debug/libarmfortas_rt.a"),
            workspace.join("target/release/libarmfortas_rt.a"),
        ];
        if seed_unrelated_archive {
            for archive in &unrelated_archives {
                fs::create_dir_all(archive.parent().unwrap())
                    .expect("create unrelated target directory");
                let ar = Command::new("ar")
                    .args(["rcs", archive.to_str().unwrap()])
                    .output()
                    .expect("create unrelated empty archive");
                assert_success(&ar, "create unrelated empty archive");
            }
        }
        let unrelated_archive_snapshots: Vec<Vec<u8>> = unrelated_archives
            .iter()
            .filter_map(|archive| fs::read(archive).ok())
            .collect();

        let source = workspace.join("hello.f90");
        fs::write(
            &source,
            b"program hello\n  print *, \"owned runtime\"\nend program hello\n",
        )
        .expect("write runtime-ownership witness");

        for opt in ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"] {
            let executable = workspace.join(format!("hello-{}", &opt[1..]));
            if seed_stale_output && opt == "-O0" {
                fs::write(&executable, b"stale executable")
                    .expect("seed stale runtime-ownership output");
            } else {
                assert!(
                    !executable.exists(),
                    "{case} {opt}: fresh runtime-ownership output must start absent"
                );
            }

            let compile = Command::new(&installed)
                .current_dir(&workspace)
                .env_remove("AFS_RUNTIME_PATH")
                .env_remove("CARGO_TARGET_DIR")
                .env("CARGO", workspace.join("cargo-must-not-run"))
                .env("TMPDIR", &runtime_tmp)
                .arg(opt)
                .arg(&source)
                .arg("-o")
                .arg(&executable)
                .output()
                .expect("launch copied armfortas in unrelated Cargo workspace");
            assert_success(
                &compile,
                &format!("copied armfortas {opt} compile in {case} workspace"),
            );

            let run = Command::new(&executable)
                .output()
                .unwrap_or_else(|err| panic!("run {opt} output from {case} workspace: {err}"));
            assert_success(
                &run,
                &format!("copied armfortas {opt} output in {case} workspace"),
            );
            assert_eq!(run.stdout, b" owned runtime\n");
            assert!(
                run.stderr.is_empty(),
                "{case} {opt}: compiled program wrote unexpected stderr"
            );

            if cfg!(target_os = "linux") && opt == "-O2" {
                let bytes = fs::read(&executable).expect("read deterministic O2 executable");
                if let Some(expected) = deterministic_o2.as_ref() {
                    assert_eq!(
                        &bytes, expected,
                        "unrelated workspace shape changed the O2 executable"
                    );
                } else {
                    deterministic_o2 = Some(bytes);
                }
            }
        }

        if seed_unrelated_archive {
            assert_eq!(
                unrelated_archives
                    .iter()
                    .map(|archive| fs::read(archive).expect("read unrelated archive"))
                    .collect::<Vec<_>>(),
                unrelated_archive_snapshots,
                "the compiler must not consume or replace unrelated archives",
            );
        } else {
            assert!(
                !workspace.join("target").exists(),
                "the compiler must not invoke Cargo in an unrelated workspace"
            );
        }
    }

    assert!(
        fs::read_dir(&runtime_tmp)
            .expect("inspect private runtime temp directory")
            .next()
            .is_none(),
        "materialized owned runtime archive was not cleaned up"
    );
}
