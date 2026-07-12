use std::path::PathBuf;
use std::process::Command;

fn compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("compiler binary 'armfortas' not built for this test profile")
}

#[test]
fn bind_c_assumed_length_character_is_rejected_at_every_opt_level() {
    const OPT_LEVELS: [&str; 6] = ["-O0", "-O1", "-O2", "-O3", "-Os", "-Ofast"];

    let dir = std::env::temp_dir().join(format!("afs_c_descriptor_diag_{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("cannot create C descriptor diagnostic directory");
    let source = dir.join("char_len.f90");
    std::fs::write(
        &source,
        r#"function char_len(text) result(n) bind(c, name="char_len")
  use iso_c_binding
  character(kind=c_char, len=*), intent(in) :: text
  integer(c_int) :: n
  n = len(text)
end function char_len
"#,
    )
    .expect("cannot write C descriptor diagnostic source");

    for opt_level in OPT_LEVELS {
        let tag = opt_level.trim_start_matches('-').to_ascii_lowercase();
        let object = dir.join(format!("char_len_{tag}.o"));
        let result = Command::new(compiler())
            .args(["-c", opt_level])
            .arg(&source)
            .arg("-o")
            .arg(&object)
            .output()
            .expect("failed to spawn armfortas");
        let stderr = String::from_utf8_lossy(&result.stderr);
        assert!(
            !result.status.success(),
            "descriptor-required source unexpectedly compiled at {opt_level}"
        );
        assert!(
            stderr.contains("BIND(C) assumed-length CHARACTER dummy 'text'")
                && stderr.contains("C descriptors are not implemented"),
            "missing descriptor diagnostic at {opt_level}: {stderr}"
        );
        assert!(
            !stderr.contains("INTERNAL COMPILER ERROR"),
            "descriptor rejection reached an internal failure at {opt_level}: {stderr}"
        );
        assert!(
            !object.exists(),
            "descriptor rejection left an object at {opt_level}"
        );
    }

    std::fs::remove_dir_all(&dir).expect("cannot remove C descriptor diagnostic directory");
}
