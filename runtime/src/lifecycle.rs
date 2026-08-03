//! Program lifecycle — init, finalize, stop.

use std::process;
use std::sync::Once;

unsafe extern "C" {
    fn atexit(cb: extern "C" fn()) -> i32;
}

static REGISTER_ATEXIT: Once = Once::new();

extern "C" fn afs_atexit_finalize() {
    crate::io_system::afs_io_finalize();
}

/// Called before the user's program body, with main's (argc, argv)
/// forwarded by the entry wrapper — both wrappers call this first,
/// while the values still sit in the argument registers. Sets up I/O
/// units and stores argv for COMMAND_ARGUMENT_COUNT/GET_COMMAND*
/// (std::env::args is empty under a non-Rust main on ELF; see
/// system.rs). Extra register garbage on old callers is harmless:
/// store_args validates before storing.
#[no_mangle]
pub extern "C" fn afs_program_init(argc: i32, argv: *const *const u8) {
    crate::system::store_args(argc, argv);
    crate::io_system::afs_io_init();
    REGISTER_ATEXIT.call_once(|| unsafe {
        let _ = atexit(afs_atexit_finalize);
    });
}

/// Called after the user's program body completes normally.
/// Flushes I/O, runs finalizers.
#[no_mangle]
pub extern "C" fn afs_program_finalize() {
    crate::io_system::afs_io_finalize();
}

/// Fortran STOP statement.
#[no_mangle]
pub extern "C" fn afs_stop() {
    afs_stop_quiet(0);
}

/// Fortran STOP statement with the F2018 QUIET= value already evaluated.
#[no_mangle]
pub extern "C" fn afs_stop_quiet(_quiet: i32) {
    afs_program_finalize();
    process::exit(0);
}

/// Fortran `STOP <int>` statement.
#[no_mangle]
pub extern "C" fn afs_stop_int(code: i64) {
    afs_stop_int_quiet(code, 0);
}

/// Fortran `STOP <int>, QUIET=...`.
#[no_mangle]
pub extern "C" fn afs_stop_int_quiet(code: i64, _quiet: i32) {
    afs_program_finalize();
    let exit_code = if (0..=255).contains(&code) {
        code as i32
    } else {
        1
    };
    process::exit(exit_code);
}

/// Fortran `STOP "message"` (character stop-code).
#[no_mangle]
pub extern "C" fn afs_stop_msg(ptr: *const u8, len: i64) {
    afs_stop_msg_quiet(ptr, len, 0);
}

/// Fortran `STOP "message", QUIET=...`.
#[no_mangle]
pub extern "C" fn afs_stop_msg_quiet(ptr: *const u8, len: i64, quiet: i32) {
    if quiet == 0 {
        if !ptr.is_null() && len > 0 {
            let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
            let msg = String::from_utf8_lossy(bytes);
            eprintln!("STOP {}", msg);
        } else {
            eprintln!("STOP");
        }
    }
    afs_program_finalize();
    process::exit(0);
}

/// Fortran ERROR STOP statement.
#[no_mangle]
pub extern "C" fn afs_error_stop() {
    afs_error_stop_quiet(0);
}

/// Fortran ERROR STOP statement with the F2018 QUIET= value already evaluated.
#[no_mangle]
pub extern "C" fn afs_error_stop_quiet(quiet: i32) {
    if quiet == 0 {
        eprintln!("ERROR STOP");
    }
    afs_program_finalize();
    process::exit(1);
}

/// Fortran `ERROR STOP "message"` (character stop-code). Prints the
/// implementation-defined banner followed by the user message — gfortran
/// emits `ERROR STOP <msg>`. Without this, `error stop "Allocation of
/// adjoint_array buffer failed."` printed only the bare banner, hiding the
/// actual diagnostic from stdlib's sort_adjoint / sort_index / many other
/// callers.
#[no_mangle]
pub extern "C" fn afs_error_stop_msg(ptr: *const u8, len: i64) {
    afs_error_stop_msg_quiet(ptr, len, 0);
}

/// Fortran `ERROR STOP "message", QUIET=...`.
#[no_mangle]
pub extern "C" fn afs_error_stop_msg_quiet(ptr: *const u8, len: i64, quiet: i32) {
    if quiet == 0 {
        if !ptr.is_null() && len > 0 {
            let bytes = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
            let msg = String::from_utf8_lossy(bytes);
            eprintln!("ERROR STOP {}", msg);
        } else {
            eprintln!("ERROR STOP");
        }
    }
    afs_program_finalize();
    process::exit(1);
}

/// Fortran `ERROR STOP <int>` (integer stop-code). Prints `ERROR STOP <n>`
/// and exits with that code (clamped to 1..=255 since Unix exit codes are
/// 8-bit). A code of 0 still produces exit 1 — `error stop 0` is meant to
/// be an abnormal termination.
#[no_mangle]
pub extern "C" fn afs_error_stop_int(code: i64) {
    afs_error_stop_int_quiet(code, 0);
}

/// Fortran `ERROR STOP <int>, QUIET=...`.
#[no_mangle]
pub extern "C" fn afs_error_stop_int_quiet(code: i64, quiet: i32) {
    if quiet == 0 {
        eprintln!("ERROR STOP {}", code);
    }
    afs_program_finalize();
    let exit_code = if code > 0 && code <= 255 {
        code as i32
    } else {
        1
    };
    process::exit(exit_code);
}
