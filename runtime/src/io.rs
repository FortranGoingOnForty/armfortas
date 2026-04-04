//! I/O subsystem — list-directed PRINT, formatted output.
//!
//! Fortran list-directed output rules:
//! - Leading space before each value
//! - Integers: right-justified in field width
//! - Reals: E-format with full precision
//! - Logicals: T or F
//! - Strings: as-is (no quotes)
//! - Newline at end of PRINT statement

use std::io::{self, Write};

/// Print a character string (list-directed).
/// Fortran passes ptr + length (no null terminator).
#[no_mangle]
pub extern "C" fn afs_print_string(ptr: *const u8, len: i64) {
    let stdout = io::stdout();
    let mut out = stdout.lock();
    let _ = out.write_all(b" ");
    if !ptr.is_null() && len > 0 {
        let slice = unsafe { std::slice::from_raw_parts(ptr, len as usize) };
        let _ = out.write_all(slice);
    }
}

/// Print a 32-bit integer (list-directed).
#[no_mangle]
pub extern "C" fn afs_print_int(val: i32) {
    print!(" {}", val);
}

/// Print a 64-bit integer (list-directed).
#[no_mangle]
pub extern "C" fn afs_print_int64(val: i64) {
    print!(" {}", val);
}

/// Print a single-precision real (list-directed, E-format).
#[no_mangle]
pub extern "C" fn afs_print_real(val: f32) {
    // Fortran list-directed: processor-dependent, typically E format.
    print!("  {:14.7E}", val);
}

/// Print a double-precision real (list-directed, E-format).
#[no_mangle]
pub extern "C" fn afs_print_real64(val: f64) {
    print!("  {:22.15E}", val);
}

/// Print a logical value (list-directed: T or F).
#[no_mangle]
pub extern "C" fn afs_print_logical(val: i32) {
    print!(" {}", if val != 0 { "T" } else { "F" });
}

/// End a PRINT statement (newline).
#[no_mangle]
pub extern "C" fn afs_print_newline() {
    println!();
}
