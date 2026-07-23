#[cfg(unix)]
mod unix {
    use std::fs;
    use std::os::unix::fs::PermissionsExt;
    use std::path::{Path, PathBuf};
    use std::process::Command;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    static NEXT_ID: AtomicU64 = AtomicU64::new(0);

    struct TempCompiler {
        dir: PathBuf,
        path: PathBuf,
    }

    impl TempCompiler {
        fn always_fails() -> Self {
            Self::create(None)
        }

        fn emits(assembly: &str) -> Self {
            Self::create(Some(assembly))
        }

        fn create(assembly: Option<&str>) -> Self {
            let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
            let nonce = SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .expect("clock after Unix epoch")
                .as_nanos();
            let dir = std::env::temp_dir().join(format!(
                "armfortas-isa-gate-test-{}-{nonce}-{id}",
                std::process::id()
            ));
            fs::create_dir(&dir).expect("create compiler stub directory");
            let path = dir.join("compiler");
            if let Some(assembly) = assembly {
                fs::write(path.with_extension("payload"), assembly)
                    .expect("write compiler stub payload");
                fs::write(
                    &path,
                    b"#!/bin/sh\n\
                      out=\n\
                      while [ \"$#\" -gt 0 ]; do\n\
                          if [ \"$1\" = -o ] && [ \"$#\" -ge 2 ]; then\n\
                              out=$2\n\
                              shift 2\n\
                          else\n\
                              shift\n\
                          fi\n\
                      done\n\
                      [ -n \"$out\" ] || exit 2\n\
                      cp \"$0.payload\" \"$out\"\n",
                )
                .expect("write compiler stub");
            } else {
                fs::write(&path, b"#!/bin/sh\nexit 1\n").expect("write compiler stub");
            }
            let mut permissions = fs::metadata(&path).unwrap().permissions();
            permissions.set_mode(0o755);
            fs::set_permissions(&path, permissions).expect("make compiler stub executable");
            Self { dir, path }
        }
    }

    impl Drop for TempCompiler {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.dir);
        }
    }

    fn run_gate(script: &str, compiler: &TempCompiler) -> std::process::Output {
        let root = Path::new(env!("CARGO_MANIFEST_DIR"));
        Command::new("sh")
            .arg(root.join(script))
            .arg(&compiler.path)
            .current_dir(root)
            .output()
            .unwrap_or_else(|err| panic!("could not run {script}: {err}"))
    }

    fn assert_gate_rejects_compiler_failure(script: &str) {
        let compiler = TempCompiler::always_fails();
        let output = run_gate(script, &compiler);
        assert!(
            !output.status.success(),
            "{script} accepted an all-failing compiler\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("clean"),
            "{script} reported clean after every compilation failed"
        );
        let stderr = String::from_utf8_lossy(&output.stderr);
        assert!(
            stderr.contains("compilation failed for"),
            "{script} did not report the erased compilation evidence:\n{stderr}"
        );
        assert!(
            stderr.contains("expected 1390 checked assemblies, got 0"),
            "{script} did not enforce the exact checked-assembly count:\n{stderr}"
        );
    }

    fn assert_gate_rejects_instruction(script: &str, instruction: &str) {
        let compiler = TempCompiler::emits(&format!(".text\n{instruction}\nret\n"));
        let output = run_gate(script, &compiler);
        assert!(
            !output.status.success(),
            "{script} accepted forbidden instruction `{instruction}`\nstdout:\n{}\nstderr:\n{}",
            String::from_utf8_lossy(&output.stdout),
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(
            !String::from_utf8_lossy(&output.stdout).contains("clean"),
            "{script} reported clean after accepting `{instruction}`"
        );
        assert!(
            String::from_utf8_lossy(&output.stderr).contains("mnemonic"),
            "{script} did not identify `{instruction}` as an ISA violation:\n{}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    #[test]
    fn x87_gate_rejects_an_all_failing_compiler() {
        assert_gate_rejects_compiler_failure("ci/check_x87.sh");
    }

    #[test]
    fn isa_ceiling_gate_rejects_an_all_failing_compiler() {
        assert_gate_rejects_compiler_failure("ci/check_isa_ceiling.sh");
    }

    #[test]
    fn isa_ceiling_gate_rejects_sse3_outside_the_baseline() {
        assert_gate_rejects_instruction("ci/check_isa_ceiling.sh", "addsubps %xmm0, %xmm1");
    }

    #[test]
    fn x87_gate_rejects_integer_loads() {
        assert_gate_rejects_instruction("ci/check_x87.sh", "fildl (%rax)");
    }
}
