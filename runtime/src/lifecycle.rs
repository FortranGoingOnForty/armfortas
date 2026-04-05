//! Program lifecycle — init, finalize, stop.

use std::process;

/// Called before the user's program body.
/// Sets up I/O units, signal handlers, etc.
#[no_mangle]
pub extern "C" fn afs_program_init() {
    crate::io_system::afs_io_init();
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
    afs_program_finalize();
    process::exit(0);
}

/// Fortran ERROR STOP statement.
#[no_mangle]
pub extern "C" fn afs_error_stop() {
    eprintln!("ERROR STOP");
    afs_program_finalize();
    process::exit(1);
}
