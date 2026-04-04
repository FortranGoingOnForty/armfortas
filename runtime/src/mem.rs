//! Memory management — allocate, deallocate, string operations.
//!
//! All heap allocation goes through these functions so we can
//! track allocations, detect leaks, and implement Fortran's
//! automatic deallocation semantics.

use std::alloc::{self, Layout};
use std::ptr;

/// Allocate `size` bytes on the heap. Returns a pointer.
/// Aborts on allocation failure (Fortran ALLOCATE with no STAT=).
#[no_mangle]
pub extern "C" fn _afs_allocate(size: i64) -> *mut u8 {
    if size <= 0 {
        return ptr::null_mut();
    }
    let layout = Layout::from_size_align(size as usize, 16)
        .expect("invalid allocation layout");
    let ptr = unsafe { alloc::alloc(layout) };
    if ptr.is_null() {
        eprintln!("ALLOCATE: out of memory ({} bytes)", size);
        std::process::exit(1);
    }
    ptr
}

/// Deallocate memory previously allocated by _afs_allocate.
#[no_mangle]
pub extern "C" fn _afs_deallocate(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    // We don't track the layout, so we use a minimum layout.
    // In a full implementation, the descriptor carries the size.
    // For now, this is a known simplification.
    let layout = Layout::from_size_align(1, 1).unwrap();
    unsafe { alloc::dealloc(ptr, layout) };
}

/// Concatenate two strings. Returns a newly allocated string.
/// Caller is responsible for freeing the result.
#[no_mangle]
pub extern "C" fn _afs_string_concat(
    a: *const u8, alen: i64,
    b: *const u8, blen: i64,
) -> *mut u8 {
    let total = (alen + blen) as usize;
    let result = _afs_allocate(total as i64);
    if !a.is_null() && alen > 0 {
        unsafe { ptr::copy_nonoverlapping(a, result, alen as usize) };
    }
    if !b.is_null() && blen > 0 {
        unsafe { ptr::copy_nonoverlapping(b, result.add(alen as usize), blen as usize) };
    }
    result
}

/// Copy a string into a fixed-length buffer, padding with spaces.
/// Used for character assignment to fixed-length variables.
#[no_mangle]
pub extern "C" fn _afs_string_copy(
    dest: *mut u8, dest_len: i64,
    src: *const u8, src_len: i64,
) {
    if dest.is_null() || dest_len <= 0 {
        return;
    }
    let copy_len = std::cmp::min(src_len, dest_len) as usize;
    if !src.is_null() && copy_len > 0 {
        unsafe { ptr::copy_nonoverlapping(src, dest, copy_len) };
    }
    // Pad remainder with spaces (Fortran character assignment rule).
    if (copy_len as i64) < dest_len {
        unsafe {
            ptr::write_bytes(dest.add(copy_len), b' ', (dest_len as usize) - copy_len);
        }
    }
}

/// Compare two strings lexicographically.
/// Returns negative, zero, or positive (like strcmp but for counted strings).
#[no_mangle]
pub extern "C" fn _afs_string_compare(
    a: *const u8, alen: i64,
    b: *const u8, blen: i64,
) -> i32 {
    let sa = if !a.is_null() && alen > 0 {
        unsafe { std::slice::from_raw_parts(a, alen as usize) }
    } else { &[] };
    let sb = if !b.is_null() && blen > 0 {
        unsafe { std::slice::from_raw_parts(b, blen as usize) }
    } else { &[] };
    sa.cmp(sb) as i32
}
