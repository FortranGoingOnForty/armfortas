use std::process::Command;

#[test]
fn scalar_pointer_array_element_target_honors_lower_bound() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=scalar_pointer_bounds test=scalar_pointer_array_element_target_honors_lower_bound count=1 reason=\"{}\"",
            reason
        );
        return;
    }

    let compiler = armfortas::testing::built_binary("armfortas")
        .expect("armfortas binary was not built for this test profile");
    let stem = format!("afs_scalar_pointer_bounds_{}", std::process::id());
    let source = std::env::temp_dir().join(format!("{stem}.f90"));
    let output = std::env::temp_dir().join(format!("{stem}.bin"));
    let runtime_cache = std::env::temp_dir().join(format!("{stem}_runtime_cache"));
    std::fs::write(
        &source,
        "program p
  implicit none
  integer, target :: fixed(-2:0)
  integer, allocatable, target :: dynamic(:)
  integer, pointer :: view
  fixed = -1
  allocate(dynamic(4:6))
  dynamic = -1
  view => fixed(-1)
  view = 42
  if (fixed(-1) /= 42) error stop 1
  if (fixed(-2) /= -1 .or. fixed(0) /= -1) error stop 2
  view => dynamic(5)
  view = 77
  if (dynamic(5) /= 77) error stop 3
  if (dynamic(4) /= -1 .or. dynamic(6) /= -1) error stop 4
  print *, 'ok'
end program
",
    )
    .expect("failed to write scalar pointer lower-bound source");

    let compile = Command::new(compiler)
        .args([
            "-O0",
            source.to_str().unwrap(),
            "-o",
            output.to_str().unwrap(),
        ])
        .env("AFS_RUNTIME_CACHE", &runtime_cache)
        .output()
        .expect("failed to spawn armfortas");
    assert!(
        compile.status.success(),
        "scalar pointer lower-bound source failed to compile: {}",
        String::from_utf8_lossy(&compile.stderr)
    );
    let run = Command::new(&output)
        .output()
        .expect("failed to run scalar pointer lower-bound binary");
    assert!(
        run.status.success(),
        "scalar pointer lower-bound binary failed:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(String::from_utf8_lossy(&run.stdout).contains("ok"));

    let _ = std::fs::remove_file(source);
    let _ = std::fs::remove_file(output);
    let _ = std::fs::remove_dir_all(runtime_cache);
}
