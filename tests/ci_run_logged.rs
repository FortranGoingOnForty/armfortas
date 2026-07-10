use std::fs;
use std::path::PathBuf;
use std::process::Command;

fn script() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("ci/run_logged.sh")
}

fn log_path(case: &str) -> PathBuf {
    std::env::temp_dir().join(format!("afs-run-logged-{}-{case}.log", std::process::id()))
}

#[test]
fn logged_command_preserves_success_and_output() {
    let log = log_path("success");
    let output = Command::new("sh")
        .arg(script())
        .arg(&log)
        .args(["sh", "-c", "printf 'successful output\\n'"])
        .output()
        .expect("run logging wrapper");

    assert!(output.status.success());
    assert_eq!(output.stdout, b"successful output\n");
    assert_eq!(fs::read(&log).expect("read captured log"), output.stdout);
    let _ = fs::remove_file(log);
}

#[test]
fn logged_command_preserves_failure_and_output() {
    let log = log_path("failure");
    let output = Command::new("sh")
        .arg(script())
        .arg(&log)
        .args(["sh", "-c", "printf 'failed output\\n'; exit 23"])
        .output()
        .expect("run logging wrapper");

    assert_eq!(output.status.code(), Some(23));
    assert_eq!(output.stdout, b"failed output\n");
    assert_eq!(fs::read(&log).expect("read captured log"), output.stdout);
    let _ = fs::remove_file(log);
}
