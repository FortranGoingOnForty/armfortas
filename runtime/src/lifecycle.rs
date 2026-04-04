//! Program lifecycle — init, finalize, stop.

use std::process;

/// Called before the user's program body.
/// Sets up I/O units, signal handlers, etc.
#[no_mangle]
pub extern "C" fn _afs_program_init() {
    // Reserved for future: I/O unit table, default signal handlers.
}

/// Called after the user's program body completes normally.
/// Flushes I/O, runs finalizers.
#[no_mangle]
pub extern "C" fn _afs_program_finalize() {
    // Flush stdout/stderr.
    use std::io::Write;
    let _ = std::io::stdout().flush();
    let _ = std::io::stderr().flush();
}

/// Fortran STOP statement.
#[no_mangle]
pub extern "C" fn _afs_stop() {
    _afs_program_finalize();
    process::exit(0);
}

/// Fortran ERROR STOP statement.
#[no_mangle]
pub extern "C" fn _afs_error_stop() {
    eprintln!("ERROR STOP");
    _afs_program_finalize();
    process::exit(1);
}
