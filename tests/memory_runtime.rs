use std::path::PathBuf;
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
    std::env::temp_dir().join(format!("afs_memory_{}_{}_{}.{}", stem, pid, id, ext))
}

fn unique_dir(stem: &str) -> PathBuf {
    let dir = unique_path(stem, "dir");
    std::fs::create_dir_all(&dir).expect("cannot create memory-runtime test directory");
    dir
}

fn write_program_in(dir: &std::path::Path, name: &str, text: &str) -> PathBuf {
    let path = dir.join(name);
    std::fs::write(&path, text).expect("cannot write memory-runtime test source");
    path
}

fn compile_program(source: &std::path::Path, output: &std::path::Path) -> std::process::Output {
    Command::new(compiler("armfortas"))
        .args([source.to_str().unwrap(), "-o", output.to_str().unwrap()])
        .output()
        .expect("failed to spawn armfortas compile")
}

#[test]
fn allocate_stat_errmsg_populates_fixed_character_target() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=allocate_stat_errmsg_populates_fixed_character_target count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("alloc_fixed_errmsg");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer :: ios\n  integer, allocatable :: a(:)\n  character(len=64) :: msg\n  msg = 'unchanged'\n  allocate(a(2), stat=ios, errmsg=msg)\n  if (ios /= 0) error stop 1\n  allocate(a(2), stat=ios, errmsg=msg)\n  if (ios == 0) error stop 2\n  if (index(trim(msg), 'ALLOCATE failed') == 0) error stop 3\n  print *, ios\n  print *, trim(msg)\nend program\n",
    );
    let exe = dir.join("alloc_fixed_errmsg.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("fixed errmsg runtime failed");
    assert!(
        run.status.success(),
        "fixed errmsg runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("2"),
        "expected nonzero STAT in fixed errmsg output: {}",
        stdout
    );
    assert!(
        stdout.contains("ALLOCATE failed"),
        "expected fixed errmsg text in output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocate_stat_errmsg_populates_deferred_character_target() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=allocate_stat_errmsg_populates_deferred_character_target count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("alloc_deferred_errmsg");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer :: ios\n  integer, allocatable :: a(:)\n  character(len=:), allocatable :: msg\n  msg = 'seed'\n  allocate(a(2), stat=ios, errmsg=msg)\n  if (ios /= 0) error stop 1\n  allocate(a(2), stat=ios, errmsg=msg)\n  if (ios == 0) error stop 2\n  if (.not. allocated(msg)) error stop 3\n  if (index(trim(msg), 'ALLOCATE failed') == 0) error stop 4\n  print *, ios\n  print *, trim(msg)\nend program\n",
    );
    let exe = dir.join("alloc_deferred_errmsg.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("deferred errmsg runtime failed");
    assert!(
        run.status.success(),
        "deferred errmsg runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("2"),
        "expected nonzero STAT in deferred errmsg output: {}",
        stdout
    );
    assert!(
        stdout.contains("ALLOCATE failed"),
        "expected deferred errmsg text in output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocate_failure_without_stat_terminates() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=allocate_failure_without_stat_terminates count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("alloc_failure_loud");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer, allocatable :: a(:)\n  allocate(a(2))\n  allocate(a(2))\n  print *, 'survived'\nend program\n",
    );
    let exe = dir.join("alloc_failure_loud.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("allocation failure runtime failed");
    assert!(
        !run.status.success(),
        "allocation failure without STAT= returned success:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("ALLOCATE"),
        "allocation failure did not report its operation:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn multi_object_allocate_preserves_first_failure() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=multi_object_allocate_preserves_first_failure count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("alloc_first_failure");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer :: ios\n  integer, allocatable :: a(:), b(:), c(:)\n  character(len=64) :: msg\n  allocate(b(1))\n  msg = 'unchanged'\n  allocate(a(1), b(1), c(1), stat=ios, errmsg=msg)\n  if (ios == 0) error stop 1\n  if (.not. allocated(a)) error stop 2\n  if (.not. allocated(b)) error stop 3\n  if (allocated(c)) error stop 4\n  if (index(trim(msg), 'ALLOCATE failed') == 0) error stop 5\n  print *, ios\n  print *, trim(msg)\nend program\n",
    );
    let exe = dir.join("alloc_first_failure.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("multi-object allocation runtime failed");
    assert!(
        run.status.success(),
        "multi-object allocation lost its first failure: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stdout).contains("ALLOCATE failed"),
        "multi-object allocation did not retain ERRMSG=:\n{}",
        String::from_utf8_lossy(&run.stdout)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocate_errmsg_requires_scalar_character_target() {
    let dir = unique_dir("alloc_bad_errmsg");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer :: ios, msg\n  integer, allocatable :: a(:)\n  allocate(a(2), stat=ios, errmsg=msg)\nend program\n",
    );
    let exe = dir.join("alloc_bad_errmsg.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        !compile.status.success(),
        "compile unexpectedly succeeded:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );
    let stderr = String::from_utf8_lossy(&compile.stderr);
    assert!(
        stderr.contains("ERRMSG=") && stderr.contains("scalar CHARACTER variable"),
        "unexpected compile failure for bad ERRMSG target: {}",
        stderr
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deallocate_stat_errmsg_reports_unallocated_array() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=deallocate_stat_errmsg_reports_unallocated_array count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("dealloc_fixed_errmsg");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer :: ios\n  integer, allocatable :: a(:)\n  character(len=64) :: msg\n  allocate(a(2))\n  msg = 'unchanged'\n  deallocate(a, stat=ios, errmsg=msg)\n  if (ios /= 0) error stop 1\n  if (trim(msg) /= 'unchanged') error stop 2\n  deallocate(a, stat=ios, errmsg=msg)\n  if (ios == 0) error stop 3\n  if (index(trim(msg), 'DEALLOCATE failed') == 0) error stop 4\n  print *, ios\n  print *, trim(msg)\nend program\n",
    );
    let exe = dir.join("dealloc_fixed_errmsg.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("fixed deallocate errmsg runtime failed");
    assert!(
        run.status.success(),
        "fixed deallocate errmsg runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("DEALLOCATE failed"),
        "expected unallocated deallocate failure in output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deallocate_unallocated_without_stat_terminates() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=deallocate_unallocated_without_stat_terminates count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("dealloc_unallocated_loud");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer, allocatable :: a(:)\n  allocate(a(2))\n  deallocate(a)\n  deallocate(a)\n  print *, 'survived'\nend program\n",
    );
    let exe = dir.join("dealloc_unallocated_loud.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("unallocated deallocation runtime failed");
    assert!(
        !run.status.success(),
        "unallocated DEALLOCATE without STAT= returned success:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("DEALLOCATE"),
        "unallocated deallocation did not report its operation:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deallocate_stat_errmsg_handles_deferred_character_paths() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=deallocate_stat_errmsg_handles_deferred_character_paths count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("dealloc_deferred_char_status");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  type :: box_t\n    character(len=:), allocatable :: text\n  end type box_t\n  type(box_t) :: box\n  integer :: ios\n  character(len=:), allocatable :: text\n  character(len=64) :: msg\n  allocate(character(len=3) :: text)\n  ios = 99\n  msg = 'unchanged'\n  deallocate(text, stat=ios, errmsg=msg)\n  if (ios /= 0) error stop 1\n  if (allocated(text)) error stop 2\n  if (trim(msg) /= 'unchanged') error stop 3\n  ios = 0\n  deallocate(text, stat=ios, errmsg=msg)\n  if (ios == 0) error stop 4\n  if (index(trim(msg), 'DEALLOCATE failed') == 0) error stop 5\n  allocate(character(len=4) :: box%text)\n  ios = 99\n  msg = 'unchanged'\n  deallocate(box%text, stat=ios, errmsg=msg)\n  if (ios /= 0) error stop 6\n  if (allocated(box%text)) error stop 7\n  if (trim(msg) /= 'unchanged') error stop 8\n  ios = 0\n  deallocate(box%text, stat=ios, errmsg=msg)\n  if (ios == 0) error stop 9\n  if (index(trim(msg), 'DEALLOCATE failed') == 0) error stop 10\n  print *, 'ok'\nend program\n",
    );
    let exe = dir.join("dealloc_deferred_char_status.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("deferred character deallocation runtime failed");
    assert!(
        run.status.success(),
        "deferred character status handling failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deallocate_stat_errmsg_handles_pointer_paths() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=deallocate_stat_errmsg_handles_pointer_paths count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("dealloc_pointer_status");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  type :: box_t\n    integer, pointer :: value\n  end type box_t\n  type(box_t) :: box\n  integer, pointer :: scalar\n  integer, pointer :: vector(:)\n  integer :: ios\n  character(len=64) :: msg\n  allocate(scalar)\n  ios = 99\n  msg = 'unchanged'\n  deallocate(scalar, stat=ios, errmsg=msg)\n  if (ios /= 0) error stop 1\n  if (associated(scalar)) error stop 2\n  if (trim(msg) /= 'unchanged') error stop 3\n  ios = 0\n  deallocate(scalar, stat=ios, errmsg=msg)\n  if (ios == 0) error stop 4\n  if (index(trim(msg), 'DEALLOCATE failed') == 0) error stop 5\n  allocate(box%value)\n  ios = 99\n  msg = 'unchanged'\n  deallocate(box%value, stat=ios, errmsg=msg)\n  if (ios /= 0) error stop 6\n  if (associated(box%value)) error stop 7\n  if (trim(msg) /= 'unchanged') error stop 8\n  ios = 0\n  deallocate(box%value, stat=ios, errmsg=msg)\n  if (ios == 0) error stop 9\n  if (index(trim(msg), 'DEALLOCATE failed') == 0) error stop 10\n  allocate(vector(2))\n  ios = 99\n  msg = 'unchanged'\n  deallocate(vector, stat=ios, errmsg=msg)\n  if (ios /= 0) error stop 11\n  if (associated(vector)) error stop 12\n  if (trim(msg) /= 'unchanged') error stop 13\n  ios = 0\n  deallocate(vector, stat=ios, errmsg=msg)\n  if (ios == 0) error stop 14\n  if (index(trim(msg), 'DEALLOCATE failed') == 0) error stop 15\n  print *, 'ok'\nend program\n",
    );
    let exe = dir.join("dealloc_pointer_status.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("pointer deallocation runtime failed");
    assert!(
        run.status.success(),
        "pointer status handling failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deallocate_unallocated_deferred_character_without_stat_terminates() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=deallocate_unallocated_deferred_character_without_stat_terminates count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("dealloc_deferred_char_loud");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  character(len=:), allocatable :: text\n  allocate(character(len=3) :: text)\n  deallocate(text)\n  deallocate(text)\n  print *, 'survived'\nend program\n",
    );
    let exe = dir.join("dealloc_deferred_char_loud.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("deferred character failure runtime failed");
    assert!(
        !run.status.success(),
        "unallocated deferred character DEALLOCATE returned success:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("DEALLOCATE"),
        "deferred character failure did not report its operation:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn deallocate_unassociated_pointer_without_stat_terminates() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=deallocate_unassociated_pointer_without_stat_terminates count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("dealloc_pointer_loud");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer, pointer :: value\n  allocate(value)\n  deallocate(value)\n  deallocate(value)\n  print *, 'survived'\nend program\n",
    );
    let exe = dir.join("dealloc_pointer_loud.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("pointer failure runtime failed");
    assert!(
        !run.status.success(),
        "unassociated pointer DEALLOCATE returned success:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    assert!(
        String::from_utf8_lossy(&run.stderr).contains("DEALLOCATE"),
        "pointer failure did not report its operation:\n{}",
        String::from_utf8_lossy(&run.stderr)
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocate_source_array_infers_shape_and_copies_values() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=allocate_source_array_infers_shape_and_copies_values count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("alloc_source_array");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer, allocatable :: a(:), b(:)\n  allocate(b(3))\n  b = [10, 20, 30]\n  allocate(a, source=b)\n  if (.not. allocated(a)) error stop 1\n  if (size(a) /= 3) error stop 2\n  if (a(1) /= b(1) .or. a(2) /= b(2) .or. a(3) /= b(3)) error stop 3\n  b(1) = 99\n  if (a(1) /= 10) error stop 4\n  print *, size(a)\n  print *, a(1), a(2), a(3)\nend program\n",
    );
    let exe = dir.join("alloc_source_array.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("source array runtime failed");
    assert!(
        run.status.success(),
        "source array runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("3"),
        "expected inferred size in output: {}",
        stdout
    );
    assert!(
        stdout.contains("10") && stdout.contains("20") && stdout.contains("30"),
        "expected copied values in output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocate_mold_array_infers_shape_without_source_copy() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=allocate_mold_array_infers_shape_without_source_copy count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("alloc_mold_array");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer, allocatable :: a(:), b(:)\n  allocate(b(4))\n  b = [1, 2, 3, 4]\n  allocate(a, mold=b)\n  if (.not. allocated(a)) error stop 1\n  if (size(a) /= 4) error stop 2\n  print *, size(a)\nend program\n",
    );
    let exe = dir.join("alloc_mold_array.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("mold array runtime failed");
    assert!(
        run.status.success(),
        "mold array runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("4"),
        "expected inferred mold size in output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocate_source_scalar_initializes_allocatable_scalar() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=allocate_source_scalar_initializes_allocatable_scalar count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("alloc_source_scalar");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer, allocatable :: x\n  allocate(x, source=7)\n  print *, allocated(x)\n  print *, x\nend program\n",
    );
    let exe = dir.join("alloc_source_scalar.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("source scalar runtime failed");
    assert!(
        run.status.success(),
        "source scalar runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("7"),
        "expected initialized scalar in output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn scalar_allocatable_value_loads_from_allocated_payload() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=scalar_allocatable_value_loads_from_allocated_payload count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("scalar_alloc_payload_load");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  real :: x(5)\n  real, allocatable :: c\n  real :: acc\n  integer :: i\n  x(1) = 1.0\n  x(2) = 2.0\n  x(3) = 3.0\n  x(4) = 4.0\n  x(5) = 5.0\n  allocate(c, source=sum(x) / real(size(x)))\n  acc = 0.0\n  do i = 1, 5\n    acc = acc + (x(i) - c) * (x(i) - c)\n  end do\n  if (.not. allocated(c)) error stop 1\n  if (abs(c - 3.0) > 1.0e-5) error stop 2\n  if (abs(acc / 5.0 - 2.0) > 1.0e-5) error stop 3\n  print *, c, acc / 5.0\nend program\n",
    );
    let exe = dir.join("scalar_alloc_payload_load.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("scalar allocatable payload load runtime failed");
    assert!(
        run.status.success(),
        "scalar allocatable payload load runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("3.0000000E0") && stdout.contains("2.0000000E0"),
        "expected scalar allocatable payload values in output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocate_component_source_array_infers_shape_and_copies_values() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=allocate_component_source_array_infers_shape_and_copies_values count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("alloc_component_source_array");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  type :: box_t\n    integer, allocatable :: vals(:)\n  end type box_t\n  type(box_t) :: box\n  integer, allocatable :: src(:)\n  allocate(src(2))\n  src = [4, 5]\n  allocate(box%vals, source=src)\n  if (.not. allocated(box%vals)) error stop 1\n  if (size(box%vals) /= 2) error stop 2\n  if (box%vals(1) /= src(1) .or. box%vals(2) /= src(2)) error stop 3\n  src(1) = 99\n  if (box%vals(1) /= 4) error stop 4\n  print *, size(box%vals)\n  print *, box%vals(1), box%vals(2)\nend program\n",
    );
    let exe = dir.join("alloc_component_source_array.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("component source array runtime failed");
    assert!(
        run.status.success(),
        "component source array runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("2"),
        "expected inferred component size in output: {}",
        stdout
    );
    assert!(
        stdout.contains("4") && stdout.contains("5"),
        "expected copied component values in output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocate_source_with_explicit_bounds_preserves_destination_shape() {
    if let Err(reason) = armfortas::testing::native_e2e_support() {
        eprintln!(
            "\nHARNESS_SKIP suite=memory_runtime test=allocate_source_with_explicit_bounds_preserves_destination_shape count=1 reason=\"{}\"",
            reason
        );
        return;
    }
    let dir = unique_dir("alloc_source_explicit_shape");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer, allocatable :: a(:), b(:)\n  allocate(b(2))\n  b = [4, 5]\n  allocate(a(2), source=b)\n  if (.not. allocated(a)) error stop 1\n  if (size(a) /= 2) error stop 2\n  if (a(1) /= 4 .or. a(2) /= 5) error stop 3\n  print *, size(a)\n  print *, a(1), a(2)\nend program\n",
    );
    let exe = dir.join("alloc_source_explicit_shape.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        compile.status.success(),
        "compile failed: {}",
        String::from_utf8_lossy(&compile.stderr)
    );

    let run = Command::new(&exe)
        .output()
        .expect("explicit-shape source runtime failed");
    assert!(
        run.status.success(),
        "explicit-shape source runtime failed: status={:?}\nstdout:\n{}\nstderr:\n{}",
        run.status,
        String::from_utf8_lossy(&run.stdout),
        String::from_utf8_lossy(&run.stderr)
    );
    let stdout = String::from_utf8_lossy(&run.stdout);
    assert!(
        stdout.contains("2") && stdout.contains("4") && stdout.contains("5"),
        "expected explicit-shape copied values in output: {}",
        stdout
    );

    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn allocate_source_and_mold_are_rejected_together() {
    let dir = unique_dir("alloc_source_mold_conflict");
    let src = write_program_in(
        &dir,
        "main.f90",
        "program p\n  implicit none\n  integer, allocatable :: a(:), b(:), c(:)\n  allocate(b(2), c(2))\n  allocate(a, source=b, mold=c)\nend program\n",
    );
    let exe = dir.join("alloc_source_mold_conflict.bin");
    let compile = compile_program(&src, &exe);
    assert!(
        !compile.status.success(),
        "compile unexpectedly succeeded:\nstdout:\n{}\nstderr:\n{}",
        String::from_utf8_lossy(&compile.stdout),
        String::from_utf8_lossy(&compile.stderr)
    );
    let stderr = String::from_utf8_lossy(&compile.stderr);
    assert!(
        stderr.contains("SOURCE=") && stderr.contains("MOLD="),
        "unexpected compile failure for SOURCE=/MOLD= conflict: {}",
        stderr
    );

    let _ = std::fs::remove_dir_all(&dir);
}
