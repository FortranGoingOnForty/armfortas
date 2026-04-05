//! Legacy I/O functions — thin wrappers around the unit-based I/O system.
//!
//! These functions are called by generated code via RuntimeFunc::Print*.
//! They delegate to the unit-based afs_write_* functions on unit 6 (stdout).

use crate::io_system;

/// Print a character string (list-directed, stdout).
#[no_mangle]
pub extern "C" fn afs_print_string(ptr: *const u8, len: i64) {
    io_system::afs_write_string(6, ptr, len);
}

/// Print a 32-bit integer (list-directed, stdout).
#[no_mangle]
pub extern "C" fn afs_print_int(val: i32) {
    io_system::afs_write_int(6, val);
}

/// Print a 64-bit integer (list-directed, stdout).
#[no_mangle]
pub extern "C" fn afs_print_int64(val: i64) {
    io_system::afs_write_int64(6, val);
}

/// Print a single-precision real (list-directed, stdout).
#[no_mangle]
pub extern "C" fn afs_print_real(val: f32) {
    io_system::afs_write_real(6, val);
}

/// Print a double-precision real (list-directed, stdout).
#[no_mangle]
pub extern "C" fn afs_print_real64(val: f64) {
    io_system::afs_write_real64(6, val);
}

/// Print a logical value (list-directed, stdout).
#[no_mangle]
pub extern "C" fn afs_print_logical(val: i32) {
    io_system::afs_write_logical(6, val);
}

/// End a PRINT statement (newline, stdout).
#[no_mangle]
pub extern "C" fn afs_print_newline() {
    io_system::afs_write_newline(6);
}
