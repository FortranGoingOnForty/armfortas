use std::fs;
use std::path::PathBuf;
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

static NEXT_TEMP_ID: AtomicUsize = AtomicUsize::new(0);

fn compiler() -> PathBuf {
    armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary should be built for integration tests")
}

fn compile_arm64_asm(source: &str, opt: &str) -> String {
    let id = NEXT_TEMP_ID.fetch_add(1, Ordering::Relaxed);
    let stem = format!("armfortas_arm64_remediation_{}_{}", std::process::id(), id);
    let source_path = std::env::temp_dir().join(format!("{stem}.f90"));
    let asm_path = std::env::temp_dir().join(format!("{stem}.s"));
    fs::write(&source_path, source).expect("write ARM64 regression source");

    let output = Command::new(compiler())
        .args(["-ffree-form", "--target", "arm64-macos", opt, "-S", "-o"])
        .arg(&asm_path)
        .arg(&source_path)
        .output()
        .expect("run armfortas ARM64 assembly compile");
    let _ = fs::remove_file(&source_path);
    assert!(
        output.status.success(),
        "ARM64 assembly compile failed: {}",
        String::from_utf8_lossy(&output.stderr)
    );

    let asm = fs::read_to_string(&asm_path).expect("read ARM64 regression assembly");
    let _ = fs::remove_file(&asm_path);
    asm
}

#[test]
fn post_call_i128_result_load_blocks_tail_call() {
    let asm = compile_arm64_asm(
        r#"
function f() result(r)
  implicit none
  integer(16) :: r
  external side
  r = 42_16
  call side()
end function
"#,
        "-O1",
    );

    let call = asm
        .find("bl _side")
        .unwrap_or_else(|| panic!("normal call must remain:\n{asm}"));
    let reload = asm[call..]
        .find("ldp x0, x1")
        .map(|offset| call + offset)
        .unwrap_or_else(|| panic!("i128 result must reload after the call:\n{asm}"));
    assert!(
        call < reload,
        "i128 result reload moved before call:\n{asm}"
    );
    assert!(
        !asm.contains("\n    b _side"),
        "call became an unsafe tail branch:\n{asm}"
    );
}
