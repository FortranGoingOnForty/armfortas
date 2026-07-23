use std::env;
use std::ffi::OsString;
use std::path::{Path, PathBuf};
use std::process::{Command, Output};

fn required_env(name: &str) -> OsString {
    env::var_os(name).unwrap_or_else(|| panic!("Cargo did not set {name}"))
}

fn rustc_command(rustc: &Path) -> Command {
    let mut command = Command::new(rustc);
    command
        .arg("--crate-name")
        .arg("armfortas_rt")
        .arg("--edition=2021")
        .arg("--crate-type=staticlib");
    command
}

fn append_encoded_rustflags(command: &mut Command) {
    let Some(flags) = env::var_os("CARGO_ENCODED_RUSTFLAGS") else {
        return;
    };
    for flag in flags.to_string_lossy().split('\u{1f}') {
        if !flag.is_empty() {
            command.arg(flag);
        }
    }
}

fn assert_rustc_success(output: Output) {
    if output.status.success() {
        return;
    }
    panic!(
        "failed to build the bundled ARMFORTAS runtime\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&output.stdout),
        String::from_utf8_lossy(&output.stderr)
    );
}

fn main() {
    println!("cargo:rerun-if-changed=src");
    println!("cargo:rerun-if-env-changed=CARGO_ENCODED_RUSTFLAGS");
    println!("cargo:rustc-check-cfg=cfg(armfortas_staticlib_payload)");

    let manifest_dir = PathBuf::from(required_env("CARGO_MANIFEST_DIR"));
    let runtime_source = manifest_dir.join("src/lib.rs");
    let output_archive = PathBuf::from(required_env("OUT_DIR")).join("libarmfortas_rt.a");
    let target = required_env("TARGET");
    let opt_level = required_env("OPT_LEVEL");
    let debug_assertions = env::var_os("CARGO_CFG_DEBUG_ASSERTIONS").is_some();

    // Stable Cargo cannot expose a staticlib dependency artifact to another
    // package target (`artifact = "staticlib"` still requires `-Z bindeps`),
    // and `cargo install` copies executables only. Compile this dependency-free
    // runtime with Cargo's target compiler and target flags, then expose that
    // exact archive through the rlib for each installable compiler binary.
    let rustc = PathBuf::from(required_env("RUSTC"));
    let mut command = rustc_command(&rustc);
    command
        .arg(&runtime_source)
        .arg("--cfg")
        .arg("armfortas_staticlib_payload")
        .arg("--target")
        .arg(target)
        .arg("-C")
        .arg(format!("opt-level={}", opt_level.to_string_lossy()))
        .arg("-C")
        .arg(format!(
            "debuginfo={}",
            if env::var_os("DEBUG").as_deref() == Some("true".as_ref()) {
                "2"
            } else {
                "0"
            }
        ))
        .arg("-C")
        .arg(format!("debug-assertions={debug_assertions}"))
        .arg(format!(
            "--remap-path-prefix={}=/armfortas",
            manifest_dir.display()
        ))
        .arg("-o")
        .arg(&output_archive);
    if let Some(panic_strategy) = env::var_os("CARGO_CFG_PANIC") {
        command
            .arg("-C")
            .arg(format!("panic={}", panic_strategy.to_string_lossy()));
    }
    append_encoded_rustflags(&mut command);

    let output = command
        .output()
        .unwrap_or_else(|err| panic!("could not launch {}: {err}", rustc.display()));
    assert_rustc_success(output);

    println!(
        "cargo:rustc-env=ARMFORTAS_BUNDLED_RUNTIME={}",
        output_archive.display()
    );
}
