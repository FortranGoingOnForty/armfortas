//! Memory management — allocate, deallocate, string operations.
//!
//! All heap allocation goes through these functions so we can
//! track allocations, detect leaks, and implement Fortran's
//! automatic deallocation semantics.

use std::ptr;

// Use libc malloc/free directly so allocate/deallocate are paired correctly
// without needing to track Rust Layout. The system allocator on macOS returns
// 16-byte aligned pointers from malloc, satisfying our alignment requirement.
extern "C" {
    fn malloc(size: usize) -> *mut u8;
    fn free(ptr: *mut u8);
}

/// Allocate `size` bytes on the heap. Returns a pointer.
/// Aborts on allocation failure (Fortran ALLOCATE with no STAT=).
#[no_mangle]
pub extern "C" fn afs_allocate(size: i64) -> *mut u8 {
    if size <= 0 {
        return ptr::null_mut();
    }
    let ptr = unsafe { malloc(size as usize) };
    if ptr.is_null() {
        eprintln!("ALLOCATE: out of memory ({} bytes)", size);
        std::process::exit(1);
    }
    ptr
}

/// Allocate storage for an explicit scalar allocatable or pointer.
///
/// The destination slot is published only on success. An existing allocation
/// or association is reported through STAT when present and terminates
/// execution otherwise.
#[no_mangle]
pub extern "C" fn afs_allocate_scalar(slot: *mut *mut u8, size: i64, stat: *mut i32) {
    let fail = |code, message: &str| {
        if !stat.is_null() {
            unsafe {
                *stat = code;
            }
            return;
        }
        eprintln!("ALLOCATE: {message}");
        std::process::exit(1);
    };

    if slot.is_null() {
        fail(1, "null scalar allocation slot");
        return;
    }
    if !unsafe { *slot }.is_null() {
        fail(2, "scalar is already allocated or associated");
        return;
    }
    let Ok(size) = usize::try_from(size) else {
        fail(4, "allocation byte count is invalid");
        return;
    };
    if size == 0 {
        fail(4, "allocation byte count is invalid");
        return;
    }

    let allocation = unsafe { malloc(size) };
    if allocation.is_null() {
        fail(3, "out of memory");
        return;
    }
    unsafe {
        *slot = allocation;
        if !stat.is_null() {
            *stat = 0;
        }
    }
}

/// Deallocate memory previously allocated by afs_allocate.
#[no_mangle]
pub extern "C" fn afs_deallocate(ptr: *mut u8) {
    if ptr.is_null() {
        return;
    }
    unsafe { free(ptr) };
}

/// Deallocate the target of an explicit scalar pointer DEALLOCATE statement.
///
/// The slot is cleared on success. An unassociated pointer is reported through
/// STAT when present and terminates execution otherwise.
#[no_mangle]
pub extern "C" fn afs_deallocate_pointer(slot: *mut *mut u8, stat: *mut i32) {
    if slot.is_null() {
        if !stat.is_null() {
            unsafe {
                *stat = 1;
            }
            return;
        }
        eprintln!("DEALLOCATE: null pointer slot");
        std::process::exit(1);
    }

    let target = unsafe { *slot };
    if target.is_null() {
        if !stat.is_null() {
            unsafe {
                *stat = 2;
            }
            return;
        }
        eprintln!("DEALLOCATE: pointer is not associated");
        std::process::exit(1);
    }

    unsafe {
        free(target);
        *slot = ptr::null_mut();
        if !stat.is_null() {
            *stat = 0;
        }
    }
}

/// Concatenate two strings. Returns a newly allocated string.
/// Caller is responsible for freeing the result.
#[no_mangle]
pub extern "C" fn afs_string_concat(a: *const u8, alen: i64, b: *const u8, blen: i64) -> *mut u8 {
    let total = (alen + blen) as usize;
    let result = afs_allocate(total as i64);
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
pub extern "C" fn afs_string_copy(dest: *mut u8, dest_len: i64, src: *const u8, src_len: i64) {
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
pub extern "C" fn afs_string_compare(a: *const u8, alen: i64, b: *const u8, blen: i64) -> i32 {
    let sa = if !a.is_null() && alen > 0 {
        unsafe { std::slice::from_raw_parts(a, alen as usize) }
    } else {
        &[]
    };
    let sb = if !b.is_null() && blen > 0 {
        unsafe { std::slice::from_raw_parts(b, blen as usize) }
    } else {
        &[]
    };
    sa.cmp(sb) as i32
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn scalar_allocation_preserves_an_existing_target_on_failure() {
        let mut slot = ptr::null_mut();
        let mut stat = 99;

        afs_allocate_scalar(&mut slot, 8, &mut stat);
        assert_eq!(stat, 0);
        assert!(!slot.is_null());
        let original = slot;

        afs_allocate_scalar(&mut slot, 8, &mut stat);
        assert_eq!(stat, 2);
        assert_eq!(slot, original);

        afs_deallocate_pointer(&mut slot, &mut stat);
        assert_eq!(stat, 0);
        assert!(slot.is_null());
    }

    #[test]
    fn scalar_allocation_rejects_invalid_sizes_without_publishing_a_target() {
        let mut slot = ptr::null_mut();
        let mut stat = 99;

        afs_allocate_scalar(&mut slot, 0, &mut stat);

        assert_eq!(stat, 4);
        assert!(slot.is_null());
    }
}
