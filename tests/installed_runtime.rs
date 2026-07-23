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
