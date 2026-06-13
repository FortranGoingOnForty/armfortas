//! Fortran character string operations.
//!
//! This is where gfortran corrupts memory on ARM64. Our implementation
//! follows one critical invariant: **always allocate new before freeing old**
//! for deferred-length assignment. This prevents use-after-free when the
//! source overlaps the destination (e.g., s = s(2:) // s(1:1)).
//!
//! Descriptors are always in memory (stack slots), never in registers.
//! The register allocator only puts the descriptor *pointer* in a register.

use crate::descriptor::*;
use std::ptr;

extern "C" {
    fn malloc(size: usize) -> *mut u8;
    fn free(ptr: *mut u8);
}

// ---- Fixed-length character assignment ----

/// Assign a source string to a fixed-length destination with space padding.
/// If src is shorter, remainder is padded with spaces.
/// If src is longer, it is truncated.
#[no_mangle]
pub extern "C" fn afs_assign_char_fixed(
    dest: *mut u8,
    dest_len: i64,
    src: *const u8,
    src_len: i64,
) {
    if dest.is_null() || dest_len <= 0 {
        return;
    }
    let copy_len = src_len.min(dest_len) as usize;
    unsafe {
        if !src.is_null() && copy_len > 0 {
            // Use ptr::copy (not copy_nonoverlapping) to handle overlapping
            // src/dest (e.g., s = s(2:) for fixed-length self-assignment).
            ptr::copy(src, dest, copy_len);
        }
        // Pad remainder with spaces.
        let pad_len = (dest_len as usize).saturating_sub(copy_len);
        if pad_len > 0 {
            ptr::write_bytes(dest.add(copy_len), b' ', pad_len);
        }
    }
}

// ---- Deferred-length character assignment (THE CRITICAL PATH) ----

/// Assign to a deferred-length character variable.
/// **Key safety property**: allocates new memory BEFORE freeing old.
/// This prevents use-after-free when src points into dest's buffer
/// (e.g., s = s(2:5) or s = s // 'more').
#[no_mangle]
pub extern "C" fn afs_assign_char_deferred(
    desc: *mut StringDescriptor,
    src: *const u8,
    src_len: i64,
) {
    if desc.is_null() {
        return;
    }
    let desc = unsafe { &mut *desc };

    if src_len <= 0 {
        desc.len = 0;
        desc.flags = STR_ALLOCATED | STR_DEFERRED;
        if !desc.is_allocated() || desc.data.is_null() {
            desc.data = ptr::null_mut();
            desc.capacity = 0;
        }
        return;
    }

    // CRITICAL: Allocate new buffer BEFORE freeing old.
    // This handles self-referential assignment (s = s(2:) // 'x').
    let needs_realloc = src_len > desc.capacity || desc.data.is_null();

    if needs_realloc {
        let new_data = unsafe { malloc(src_len as usize) };
        if new_data.is_null() {
            eprintln!("character assignment: out of memory ({} bytes)", src_len);
            std::process::exit(1);
        }

        // Copy source to new buffer. Use `ptr::copy` rather than
        // `copy_nonoverlapping`: the runtime intentionally accepts
        // self-referential/alias-heavy deferred-length assignment
        // shapes, and some compiler paths can still route an
        // overlapping slice through the reallocating branch.
        if !src.is_null() {
            unsafe {
                ptr::copy(src, new_data, src_len as usize);
            }
        }

        // NOW free old buffer (after copy is complete).
        if desc.is_allocated() && !desc.data.is_null() {
            unsafe {
                free(desc.data);
            }
        }

        desc.data = new_data;
        desc.capacity = src_len;
    } else {
        // Fits in existing buffer. Use ptr::copy (not copy_nonoverlapping)
        // in case src overlaps with dest (e.g., s = s(2:)).
        if !src.is_null() {
            unsafe {
                ptr::copy(src, desc.data, src_len as usize);
            }
        }
    }

    desc.len = src_len;
    desc.flags |= STR_ALLOCATED | STR_DEFERRED;
}

/// Deallocate a deferred-length string descriptor.
#[no_mangle]
pub extern "C" fn afs_dealloc_string(desc: *mut StringDescriptor) {
    if desc.is_null() {
        return;
    }
    let desc = unsafe { &mut *desc };
    if desc.is_allocated() && !desc.data.is_null() {
        unsafe {
            free(desc.data);
        }
    }
    desc.data = ptr::null_mut();
    desc.len = 0;
    desc.capacity = 0;
    // Preserve STR_DEFERRED — the variable is still character(:), allocatable
    // after DEALLOCATE. Clear only STR_ALLOCATED.
    desc.flags &= STR_DEFERRED; // keep deferred, clear everything else
}

/// ALLOCATED(s) for deferred-length character descriptors.
#[no_mangle]
pub extern "C" fn afs_string_allocated(desc: *const StringDescriptor) -> i32 {
    if desc.is_null() {
        return 0;
    }
    unsafe { (*desc).is_allocated() as i32 }
}

/// Transfer allocation from `from` to `to` (F2003 MOVE_ALLOC).
///
/// `to` is deallocated if allocated, then receives `from`'s descriptor.
/// `from` is cleared back to an unallocated deferred-length descriptor.
#[no_mangle]
pub extern "C" fn afs_move_alloc_string(from: *mut StringDescriptor, to: *mut StringDescriptor) {
    if from.is_null() || to.is_null() {
        return;
    }

    let from_desc = unsafe { &mut *from };
    let to_desc = unsafe { &mut *to };

    if to_desc.is_allocated() && !to_desc.data.is_null() {
        unsafe {
            free(to_desc.data);
        }
    }

    *to_desc = from_desc.clone();

    from_desc.data = ptr::null_mut();
    from_desc.len = 0;
    from_desc.capacity = 0;
    from_desc.flags &= STR_DEFERRED;
}

// ---- Concatenation ----

/// Concatenate two strings into a pre-allocated result buffer.
/// The caller must ensure result has at least a_len + b_len bytes.
#[no_mangle]
pub extern "C" fn afs_concat(result: *mut u8, a: *const u8, a_len: i64, b: *const u8, b_len: i64) {
    if result.is_null() {
        return;
    }
    unsafe {
        if !a.is_null() && a_len > 0 {
            // Some compiler paths still materialize concatenation into a
            // buffer that aliases one of the source slices. `ptr::copy`
            // tolerates overlap and keeps the runtime from tripping Rust's
            // UB guards on otherwise valid self-referential Fortran shapes.
            ptr::copy(a, result, a_len as usize);
        }
        if !b.is_null() && b_len > 0 {
            ptr::copy(b, result.add(a_len as usize), b_len as usize);
        }
    }
}

// ---- Comparison ----

/// Compare two Fortran character strings.
/// Shorter string is padded with spaces for comparison (Fortran standard).
/// Returns: -1 if a < b, 0 if a == b, 1 if a > b.
#[no_mangle]
pub extern "C" fn afs_compare_char(a: *const u8, a_len: i64, b: *const u8, b_len: i64) -> i32 {
    let max_len = a_len.max(b_len) as usize;
    for i in 0..max_len {
        let ac = if i < a_len as usize && !a.is_null() {
            unsafe { *a.add(i) }
        } else {
            b' '
        };
        let bc = if i < b_len as usize && !b.is_null() {
            unsafe { *b.add(i) }
        } else {
            b' '
        };
        if ac < bc {
            return -1;
        }
        if ac > bc {
            return 1;
        }
    }
    0
}

// ---- String intrinsics ----

/// TRIM: return length of string without trailing spaces.
/// The data pointer is unchanged (TRIM returns a view, not a copy).
#[no_mangle]
pub extern "C" fn afs_len_trim(src: *const u8, src_len: i64) -> i64 {
    if src.is_null() || src_len <= 0 {
        return 0;
    }
    let slice = unsafe { std::slice::from_raw_parts(src, src_len as usize) };
    let trimmed = slice
        .iter()
        .rposition(|&b| b != b' ')
        .map(|pos| pos + 1)
        .unwrap_or(0);
    trimmed as i64
}

/// ADJUSTL: left-justify by removing leading spaces, padding trailing.
#[no_mangle]
pub extern "C" fn afs_adjustl(dest: *mut u8, src: *const u8, len: i64) {
    if dest.is_null() || src.is_null() || len <= 0 {
        return;
    }
    let slice = unsafe { std::slice::from_raw_parts(src, len as usize) };
    let leading = slice
        .iter()
        .position(|&b| b != b' ')
        .unwrap_or(len as usize);
    let content_len = (len as usize) - leading;
    unsafe {
        if content_len > 0 {
            ptr::copy(src.add(leading), dest, content_len);
        }
        if leading > 0 {
            ptr::write_bytes(dest.add(content_len), b' ', leading);
        }
    }
}

/// ADJUSTR: right-justify by removing trailing spaces, padding leading.
#[no_mangle]
pub extern "C" fn afs_adjustr(dest: *mut u8, src: *const u8, len: i64) {
    if dest.is_null() || src.is_null() || len <= 0 {
        return;
    }
    let slice = unsafe { std::slice::from_raw_parts(src, len as usize) };
    let trailing = slice
        .iter()
        .rposition(|&b| b != b' ')
        .map(|pos| (len as usize) - pos - 1)
        .unwrap_or(len as usize);
    let content_len = (len as usize) - trailing;
    unsafe {
        if trailing > 0 {
            ptr::write_bytes(dest, b' ', trailing);
        }
        if content_len > 0 {
            ptr::copy(src, dest.add(trailing), content_len);
        }
    }
}

/// INDEX: find first (or last if back=1) occurrence of substring.
/// Returns 1-based position, or 0 if not found.
#[no_mangle]
pub extern "C" fn afs_c_strlen(src: *const u8) -> i64 {
    if src.is_null() {
        return 0;
    }
    let mut len: i64 = 0;
    unsafe {
        while *src.add(len as usize) != 0 {
            len += 1;
        }
    }
    len
}

/// INDEX: find first (or last if back=1) occurrence of substring.
/// Returns 1-based position, or 0 if not found.
#[no_mangle]
pub extern "C" fn afs_index(
    str_ptr: *const u8,
    str_len: i64,
    sub_ptr: *const u8,
    sub_len: i64,
    back: i32,
) -> i64 {
    if str_ptr.is_null() || sub_ptr.is_null() || str_len <= 0 {
        return 0;
    }
    if sub_len <= 0 {
        return if back != 0 { str_len + 1 } else { 1 };
    }
    if sub_len > str_len {
        return 0;
    }

    let haystack = unsafe { std::slice::from_raw_parts(str_ptr, str_len as usize) };
    let needle = unsafe { std::slice::from_raw_parts(sub_ptr, sub_len as usize) };

    if back != 0 {
        // Search from right.
        for i in (0..=(str_len - sub_len) as usize).rev() {
            if &haystack[i..i + needle.len()] == needle {
                return (i + 1) as i64; // 1-based
            }
        }
    } else {
        for i in 0..=(str_len - sub_len) as usize {
            if &haystack[i..i + needle.len()] == needle {
                return (i + 1) as i64;
            }
        }
    }
    0
}

/// SCAN: find first (or last) character from set in string.
/// Returns 1-based position, or 0 if not found.
#[no_mangle]
pub extern "C" fn afs_scan(
    str_ptr: *const u8,
    str_len: i64,
    set_ptr: *const u8,
    set_len: i64,
    back: i32,
) -> i64 {
    if str_ptr.is_null() || set_ptr.is_null() || str_len <= 0 || set_len <= 0 {
        return 0;
    }
    let s = unsafe { std::slice::from_raw_parts(str_ptr, str_len as usize) };
    let set = unsafe { std::slice::from_raw_parts(set_ptr, set_len as usize) };

    if back != 0 {
        for (i, &c) in s.iter().enumerate().rev() {
            if set.contains(&c) {
                return (i + 1) as i64;
            }
        }
    } else {
        for (i, &c) in s.iter().enumerate() {
            if set.contains(&c) {
                return (i + 1) as i64;
            }
        }
    }
    0
}

/// VERIFY: find first (or last) character NOT in set.
/// Returns 1-based position, or 0 if all characters are in set.
#[no_mangle]
pub extern "C" fn afs_verify(
    str_ptr: *const u8,
    str_len: i64,
    set_ptr: *const u8,
    set_len: i64,
    back: i32,
) -> i64 {
    if str_ptr.is_null() || str_len <= 0 {
        return 0;
    }
    let s = unsafe { std::slice::from_raw_parts(str_ptr, str_len as usize) };
    let set = if !set_ptr.is_null() && set_len > 0 {
        unsafe { std::slice::from_raw_parts(set_ptr, set_len as usize) }
    } else {
        &[]
    };

    if back != 0 {
        for (i, &c) in s.iter().enumerate().rev() {
            if !set.contains(&c) {
                return (i + 1) as i64;
            }
        }
    } else {
        for (i, &c) in s.iter().enumerate() {
            if !set.contains(&c) {
                return (i + 1) as i64;
            }
        }
    }
    0
}

/// REPEAT: repeat string ncopies times into dest.
/// Dest must have at least src_len * ncopies bytes.
#[no_mangle]
pub extern "C" fn afs_repeat(src: *const u8, src_len: i64, ncopies: i64, dest: *mut u8) {
    if dest.is_null() || src.is_null() || src_len <= 0 || ncopies <= 0 {
        return;
    }
    unsafe {
        for i in 0..ncopies as usize {
            ptr::copy_nonoverlapping(src, dest.add(i * src_len as usize), src_len as usize);
        }
    }
}

/// CHAR: integer to character (single byte).
#[no_mangle]
pub extern "C" fn afs_char(i: i32) -> u8 {
    (i & 0xFF) as u8
}

/// ICHAR: character to integer.
#[no_mangle]
pub extern "C" fn afs_ichar(c: u8) -> i32 {
    c as i32
}

/// ICHAR from an addressable character byte.
#[no_mangle]
pub extern "C" fn afs_ichar_ptr(c: *const u8) -> i32 {
    if c.is_null() {
        return 0;
    }
    unsafe { *c as i32 }
}

/// LGE: lexicographic greater-than-or-equal (ASCII collating sequence).
#[no_mangle]
pub extern "C" fn afs_lge(a: *const u8, a_len: i64, b: *const u8, b_len: i64) -> i32 {
    (afs_compare_char(a, a_len, b, b_len) >= 0) as i32
}

/// LGT: lexicographic greater-than.
#[no_mangle]
pub extern "C" fn afs_lgt(a: *const u8, a_len: i64, b: *const u8, b_len: i64) -> i32 {
    (afs_compare_char(a, a_len, b, b_len) > 0) as i32
}

/// LLE: lexicographic less-than-or-equal.
#[no_mangle]
pub extern "C" fn afs_lle(a: *const u8, a_len: i64, b: *const u8, b_len: i64) -> i32 {
    (afs_compare_char(a, a_len, b, b_len) <= 0) as i32
}

/// LLT: lexicographic less-than.
#[no_mangle]
pub extern "C" fn afs_llt(a: *const u8, a_len: i64, b: *const u8, b_len: i64) -> i32 {
    (afs_compare_char(a, a_len, b, b_len) < 0) as i32
}

/// F2023 16.9.197 SPLIT(STRING, SET, POS [, BACK]). POS is 1-based
/// iteration state, updated in place. Each character of SET is a
/// separator.
///
/// Forward (back == 0): on entry 0 <= POS <= LEN; sets POS to the
/// position of the first separator strictly after the old POS, or
/// LEN+1 if there is none. The token just parsed is
/// STRING(old_pos+1 : new_pos-1).
///
/// Backward (back != 0): on entry 1 <= POS <= LEN+1; sets POS to the
/// position of the last separator strictly before the old POS, or 0
/// if none. The token is STRING(new_pos+1 : old_pos-1).
#[no_mangle]
pub extern "C" fn afs_split(
    str_ptr: *const u8,
    str_len: i64,
    set_ptr: *const u8,
    set_len: i64,
    pos: *mut i64,
    back: i32,
) {
    if pos.is_null() {
        return;
    }
    let cur = unsafe { *pos };
    let s: &[u8] = if str_ptr.is_null() || str_len <= 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(str_ptr, str_len as usize) }
    };
    let set: &[u8] = if set_ptr.is_null() || set_len <= 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(set_ptr, set_len as usize) }
    };
    let is_sep = |c: u8| set.contains(&c);

    let new_pos = if back == 0 {
        // First separator at a 1-based position strictly greater than cur.
        let mut p = cur.max(0) + 1;
        let mut found = str_len + 1;
        while p <= str_len {
            if is_sep(s[(p - 1) as usize]) {
                found = p;
                break;
            }
            p += 1;
        }
        found
    } else {
        // Last separator at a 1-based position strictly less than cur.
        let mut p = cur.min(str_len + 1) - 1;
        let mut found = 0i64;
        while p >= 1 {
            if is_sep(s[(p - 1) as usize]) {
                found = p;
                break;
            }
            p -= 1;
        }
        found
    };
    unsafe {
        *pos = new_pos;
    }
}

// ---- TOKENIZE (F2023 16.9.211) ----

/// Token boundaries for STRING split on SET. Returns 1-based (start,
/// end) pairs; an empty token has end == start-1. Mirrors flang's
/// AnalyzeTokenize: empty STRING yields one empty token, empty SET
/// yields one token equal to STRING, otherwise token count is the
/// separator count plus one (empty tokens at boundaries and between
/// consecutive separators).
fn tokenize_bounds(s: &[u8], set: &[u8]) -> Vec<(i64, i64)> {
    let mut out = Vec::new();
    if s.is_empty() {
        out.push((1, 0));
        return out;
    }
    if set.is_empty() {
        out.push((1, s.len() as i64));
        return out;
    }
    let mut tok_start = 0usize; // 0-based start of current token
    for (pos, &c) in s.iter().enumerate() {
        if set.contains(&c) {
            // token chars are s[tok_start..pos]; 1-based start..end
            out.push((tok_start as i64 + 1, pos as i64));
            tok_start = pos + 1;
        }
    }
    out.push((tok_start as i64 + 1, s.len() as i64));
    out
}

fn tokenize_slice<'a>(ptr: *const u8, len: i64) -> &'a [u8] {
    if ptr.is_null() || len <= 0 {
        &[]
    } else {
        unsafe { std::slice::from_raw_parts(ptr, len as usize) }
    }
}

/// Allocate a rank-1 array descriptor, first deallocating it if it is
/// already allocated. TOKENIZE's output arrays are INTENT(OUT)
/// allocatables — automatically deallocated on entry — so reusing the
/// same variable across calls must not trip the "already allocated"
/// guard.
unsafe fn tokenize_realloc_1d(desc: *mut ArrayDescriptor, elem_size: i64, n: i64) {
    if !desc.is_null() && (*desc).is_allocated() {
        crate::array::afs_deallocate_array(desc, ptr::null_mut());
    }
    crate::array::afs_allocate_1d(desc, elem_size, n);
}

unsafe fn tokenize_store_int(base: *mut u8, idx: usize, kind: i64, value: i64) {
    let p = base.add(idx * kind as usize);
    match kind {
        1 => *(p as *mut i8) = value as i8,
        2 => *(p as *mut i16) = value as i16,
        8 => *(p as *mut i64) = value,
        _ => *(p as *mut i32) = value as i32,
    }
}

/// TOKENIZE Form 2: `CALL TOKENIZE(STRING, SET, FIRST, LAST)`. FIRST
/// and LAST are allocatable rank-1 integer arrays (kind = `int_kind`
/// bytes); both are allocated to the token count and filled with the
/// 1-based start/end positions of each token.
#[no_mangle]
pub extern "C" fn afs_tokenize_positions(
    str_ptr: *const u8,
    str_len: i64,
    set_ptr: *const u8,
    set_len: i64,
    first: *mut ArrayDescriptor,
    last: *mut ArrayDescriptor,
    int_kind: i64,
) {
    if first.is_null() || last.is_null() {
        return;
    }
    let s = tokenize_slice(str_ptr, str_len);
    let set = tokenize_slice(set_ptr, set_len);
    let bounds = tokenize_bounds(s, set);
    let n = bounds.len() as i64;
    let kind = if int_kind <= 0 { 4 } else { int_kind };
    unsafe {
        tokenize_realloc_1d(first, kind, n);
        tokenize_realloc_1d(last, kind, n);
        let fbase = (*first).base_addr;
        let lbase = (*last).base_addr;
        for (i, &(start, end)) in bounds.iter().enumerate() {
            tokenize_store_int(fbase, i, kind, start);
            tokenize_store_int(lbase, i, kind, end);
        }
    }
}

/// TOKENIZE Form 1: `CALL TOKENIZE(STRING, SET, TOKENS [, SEPARATOR])`.
/// TOKENS is an allocatable rank-1 deferred-length character array;
/// it is allocated to the token count with element length equal to the
/// longest token (shorter tokens space-padded). If `separator` is
/// non-null it is allocated to count-1 single-character elements
/// holding the separator that ended each token.
#[no_mangle]
pub extern "C" fn afs_tokenize_tokens(
    str_ptr: *const u8,
    str_len: i64,
    set_ptr: *const u8,
    set_len: i64,
    tokens: *mut ArrayDescriptor,
    separator: *mut ArrayDescriptor,
    char_kind: i64,
) {
    if tokens.is_null() {
        return;
    }
    let s = tokenize_slice(str_ptr, str_len);
    let set = tokenize_slice(set_ptr, set_len);
    let bounds = tokenize_bounds(s, set);
    let n = bounds.len() as i64;
    let ck = if char_kind <= 0 { 1 } else { char_kind };
    let max_len = bounds
        .iter()
        .map(|&(start, end)| (end - start + 1).max(0))
        .max()
        .unwrap_or(0);
    let elem_size = max_len * ck;
    unsafe {
        tokenize_realloc_1d(tokens, elem_size, n);
        let base = (*tokens).base_addr;
        if elem_size > 0 && !base.is_null() {
            for (i, &(start, end)) in bounds.iter().enumerate() {
                let dest = base.add(i * elem_size as usize);
                let tok_len = ((end - start + 1).max(0)) as usize;
                if tok_len > 0 {
                    // s is 0-based; token chars are s[start-1 .. end]
                    ptr::copy_nonoverlapping(
                        s.as_ptr().add((start - 1) as usize),
                        dest,
                        tok_len * ck as usize,
                    );
                }
                let pad = elem_size as usize - tok_len * ck as usize;
                if pad > 0 {
                    ptr::write_bytes(dest.add(tok_len * ck as usize), b' ', pad);
                }
            }
        }
    }
    // SEPARATOR: one single-character element per inter-token boundary.
    if !separator.is_null() {
        let sep_count = if n > 0 { n - 1 } else { 0 };
        unsafe {
            tokenize_realloc_1d(separator, ck, sep_count);
        }
        if sep_count > 0 && !set.is_empty() {
            unsafe {
                let sbase = (*separator).base_addr;
                if !sbase.is_null() {
                    // The separators are the in-SET characters of STRING,
                    // in order — exactly the boundaries between tokens.
                    let mut k = 0usize;
                    for &c in s.iter() {
                        if set.contains(&c) {
                            if k >= sep_count as usize {
                                break;
                            }
                            *sbase.add(k * ck as usize) = c;
                            k += 1;
                        }
                    }
                }
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // ---- TOKENIZE ----

    #[test]
    fn tokenize_bounds_basic() {
        assert_eq!(
            tokenize_bounds(b"a,bb,ccc", b","),
            vec![(1, 1), (3, 4), (6, 8)]
        );
    }

    #[test]
    fn tokenize_bounds_empty_tokens() {
        // ",x,,y," → "", "x", "", "y", "" with LAST=FIRST-1 for empties.
        assert_eq!(
            tokenize_bounds(b",x,,y,", b","),
            vec![(1, 0), (2, 2), (4, 3), (5, 5), (7, 6)]
        );
    }

    #[test]
    fn tokenize_bounds_edge_cases() {
        assert_eq!(tokenize_bounds(b"", b","), vec![(1, 0)]); // empty string → 1 empty token
        assert_eq!(tokenize_bounds(b"abc", b""), vec![(1, 3)]); // empty set → whole string
    }

    #[test]
    fn tokenize_positions_form2() {
        let s = b"a,bb,ccc";
        let mut first = ArrayDescriptor::zeroed();
        let mut last = ArrayDescriptor::zeroed();
        afs_tokenize_positions(s.as_ptr(), 8, b",".as_ptr(), 1, &mut first, &mut last, 4);
        assert_eq!(first.dims[0].upper_bound, 3);
        unsafe {
            let f = std::slice::from_raw_parts(first.base_addr as *const i32, 3);
            let l = std::slice::from_raw_parts(last.base_addr as *const i32, 3);
            assert_eq!(f, &[1, 3, 6]);
            assert_eq!(l, &[1, 4, 8]);
        }
    }

    #[test]
    fn tokenize_tokens_form1() {
        let s = b"a,bb,ccc";
        let mut tokens = ArrayDescriptor::zeroed();
        let mut sep = ArrayDescriptor::zeroed();
        afs_tokenize_tokens(s.as_ptr(), 8, b",".as_ptr(), 1, &mut tokens, &mut sep, 1);
        assert_eq!(tokens.dims[0].upper_bound, 3);
        assert_eq!(tokens.elem_size, 3); // maxTokenLen = "ccc"
        unsafe {
            let data = std::slice::from_raw_parts(tokens.base_addr, 9);
            // "a  ", "bb ", "ccc" — each padded to 3.
            assert_eq!(data, b"a  bb ccc");
            let seps = std::slice::from_raw_parts(sep.base_addr, 2);
            assert_eq!(seps, b",,");
        }
    }

    // ---- SPLIT ----

    fn split_forward_tokens(s: &[u8], set: &[u8]) -> Vec<Vec<u8>> {
        // Canonical F2023 forward loop: the exit test (POS > LEN) runs
        // after processing each token, so a trailing separator still
        // yields a final empty token.
        let mut pos: i64 = 0;
        let mut tokens = Vec::new();
        loop {
            let istart = pos + 1;
            afs_split(
                s.as_ptr(),
                s.len() as i64,
                set.as_ptr(),
                set.len() as i64,
                &mut pos,
                0,
            );
            tokens.push(s[(istart - 1) as usize..(pos - 1) as usize].to_vec());
            if pos > s.len() as i64 {
                break;
            }
        }
        tokens
    }

    #[test]
    fn split_forward_csv() {
        let toks = split_forward_tokens(b"a,bb,ccc", b",");
        assert_eq!(toks, vec![b"a".to_vec(), b"bb".to_vec(), b"ccc".to_vec()]);
    }

    #[test]
    fn split_forward_empty_tokens() {
        // Leading, doubled, and trailing separators yield empty tokens.
        let toks = split_forward_tokens(b",a,,b,", b",");
        assert_eq!(
            toks,
            vec![
                b"".to_vec(),
                b"a".to_vec(),
                b"".to_vec(),
                b"b".to_vec(),
                b"".to_vec()
            ]
        );
    }

    #[test]
    fn split_not_found_forward_is_len_plus_1() {
        let s = b"abc";
        let mut pos: i64 = 0;
        afs_split(s.as_ptr(), 3, b",".as_ptr(), 1, &mut pos, 0);
        assert_eq!(pos, 4);
    }

    #[test]
    fn split_backward_walk() {
        let s = b"a,bb,ccc";
        let mut pos: i64 = s.len() as i64 + 1;
        let mut toks: Vec<Vec<u8>> = Vec::new();
        while pos > 0 {
            let old = pos;
            afs_split(s.as_ptr(), s.len() as i64, b",".as_ptr(), 1, &mut pos, 1);
            toks.push(s[pos as usize..(old - 1) as usize].to_vec());
        }
        assert_eq!(toks, vec![b"ccc".to_vec(), b"bb".to_vec(), b"a".to_vec()]);
    }

    #[test]
    fn split_not_found_backward_is_zero() {
        let s = b"abc";
        let mut pos: i64 = 4;
        afs_split(s.as_ptr(), 3, b",".as_ptr(), 1, &mut pos, 1);
        assert_eq!(pos, 0);
    }

    // ---- Fixed-length assignment ----

    #[test]
    fn fixed_assign_shorter_src() {
        let mut dest = [0u8; 10];
        let src = b"hello";
        afs_assign_char_fixed(dest.as_mut_ptr(), 10, src.as_ptr(), 5);
        assert_eq!(&dest, b"hello     ");
    }

    #[test]
    fn fixed_assign_longer_src() {
        let mut dest = [0u8; 5];
        let src = b"hello world";
        afs_assign_char_fixed(dest.as_mut_ptr(), 5, src.as_ptr(), 11);
        assert_eq!(&dest, b"hello");
    }

    #[test]
    fn fixed_assign_exact_length() {
        let mut dest = [0u8; 5];
        let src = b"hello";
        afs_assign_char_fixed(dest.as_mut_ptr(), 5, src.as_ptr(), 5);
        assert_eq!(&dest, b"hello");
    }

    // ---- Deferred-length assignment (THE CRITICAL TESTS) ----

    #[test]
    fn deferred_assign_initial() {
        let mut desc = StringDescriptor::zeroed();
        let src = b"hello";
        afs_assign_char_deferred(&mut desc, src.as_ptr(), 5);
        assert!(desc.is_allocated());
        assert_eq!(desc.len, 5);
        let data = unsafe { std::slice::from_raw_parts(desc.data, 5) };
        assert_eq!(data, b"hello");
        afs_dealloc_string(&mut desc);
    }

    #[test]
    fn deferred_assign_realloc_larger() {
        let mut desc = StringDescriptor::zeroed();
        afs_assign_char_deferred(&mut desc, b"hi".as_ptr(), 2);
        assert_eq!(desc.len, 2);
        afs_assign_char_deferred(&mut desc, b"hello world".as_ptr(), 11);
        assert_eq!(desc.len, 11);
        assert!(desc.capacity >= 11);
        let data = unsafe { std::slice::from_raw_parts(desc.data, 11) };
        assert_eq!(data, b"hello world");
        afs_dealloc_string(&mut desc);
    }

    #[test]
    fn deferred_assign_fits_in_existing() {
        let mut desc = StringDescriptor::zeroed();
        afs_assign_char_deferred(&mut desc, b"hello world".as_ptr(), 11);
        let old_ptr = desc.data;
        afs_assign_char_deferred(&mut desc, b"hi".as_ptr(), 2);
        // Should reuse buffer (capacity >= 2).
        assert_eq!(desc.data, old_ptr); // same buffer
        assert_eq!(desc.len, 2);
        let data = unsafe { std::slice::from_raw_parts(desc.data, 2) };
        assert_eq!(data, b"hi");
        afs_dealloc_string(&mut desc);
    }

    #[test]
    fn deferred_assign_empty() {
        let mut desc = StringDescriptor::zeroed();
        afs_assign_char_deferred(&mut desc, b"hello".as_ptr(), 5);
        let old_data = desc.data;
        let old_capacity = desc.capacity;
        afs_assign_char_deferred(&mut desc, ptr::null(), 0);
        assert!(desc.is_allocated());
        assert_eq!(afs_string_allocated(&desc), 1);
        assert_eq!(desc.len, 0);
        assert_eq!(desc.data, old_data);
        assert_eq!(desc.capacity, old_capacity);
        afs_dealloc_string(&mut desc);
        assert_eq!(afs_string_allocated(&desc), 0);
    }

    #[test]
    fn deferred_self_referential_safe() {
        // s = s(2:5) — source points into dest's buffer.
        let mut desc = StringDescriptor::zeroed();
        afs_assign_char_deferred(&mut desc, b"hello world".as_ptr(), 11);

        // Simulate s = s(2:5): source is desc.data + 1, len = 4.
        let src_ptr = unsafe { desc.data.add(1) };
        afs_assign_char_deferred(&mut desc, src_ptr, 4);

        assert_eq!(desc.len, 4);
        let data = unsafe { std::slice::from_raw_parts(desc.data, 4) };
        assert_eq!(data, b"ello");
        afs_dealloc_string(&mut desc);
    }

    #[test]
    fn deferred_self_referential_realloc_safe() {
        // Force the aliasing path through the "needs_realloc" branch by
        // shrinking capacity metadata while keeping the backing buffer valid.
        let mut desc = StringDescriptor::zeroed();
        afs_assign_char_deferred(&mut desc, b"abcdef".as_ptr(), 6);
        desc.capacity = 1;

        let src_ptr = unsafe { desc.data.add(1) };
        afs_assign_char_deferred(&mut desc, src_ptr, 4);

        assert_eq!(desc.len, 4);
        let data = unsafe { std::slice::from_raw_parts(desc.data, 4) };
        assert_eq!(data, b"bcde");
        afs_dealloc_string(&mut desc);
    }

    #[test]
    fn move_alloc_transfers_deferred_string_storage() {
        let mut from = StringDescriptor::zeroed();
        let mut to = StringDescriptor::zeroed();

        afs_assign_char_deferred(&mut from, b"hello".as_ptr(), 5);
        afs_assign_char_deferred(&mut to, b"bye".as_ptr(), 3);

        afs_move_alloc_string(&mut from, &mut to);

        assert!(!from.is_allocated());
        assert!(from.data.is_null());
        assert_eq!(from.len, 0);
        assert_eq!(from.capacity, 0);

        assert!(to.is_allocated());
        let data = unsafe { std::slice::from_raw_parts(to.data, 5) };
        assert_eq!(data, b"hello");

        afs_dealloc_string(&mut to);
    }

    #[test]
    fn string_allocated_reflects_descriptor_state() {
        let mut desc = StringDescriptor::zeroed();
        assert_eq!(afs_string_allocated(&desc), 0);
        afs_assign_char_deferred(&mut desc, b"abc".as_ptr(), 3);
        assert_eq!(afs_string_allocated(&desc), 1);
        afs_dealloc_string(&mut desc);
        assert_eq!(afs_string_allocated(&desc), 0);
    }

    // ---- Concatenation ----

    #[test]
    fn concat_basic() {
        let mut result = [0u8; 11];
        afs_concat(
            result.as_mut_ptr(),
            b"hello".as_ptr(),
            5,
            b" world".as_ptr(),
            6,
        );
        assert_eq!(&result, b"hello world");
    }

    #[test]
    fn concat_tolerates_result_aliasing_lhs() {
        let mut result = [0u8; 6];
        result[..3].copy_from_slice(b"abc");
        afs_concat(result.as_mut_ptr(), result.as_ptr(), 3, b"def".as_ptr(), 3);
        assert_eq!(&result, b"abcdef");
    }

    // ---- Comparison ----

    #[test]
    fn compare_equal() {
        assert_eq!(
            afs_compare_char(b"hello".as_ptr(), 5, b"hello".as_ptr(), 5),
            0
        );
    }

    #[test]
    fn compare_less() {
        assert_eq!(afs_compare_char(b"abc".as_ptr(), 3, b"abd".as_ptr(), 3), -1);
    }

    #[test]
    fn compare_greater() {
        assert_eq!(afs_compare_char(b"abd".as_ptr(), 3, b"abc".as_ptr(), 3), 1);
    }

    #[test]
    fn compare_with_padding() {
        // "abc" vs "abc   " — should be equal (space padding).
        assert_eq!(
            afs_compare_char(b"abc".as_ptr(), 3, b"abc   ".as_ptr(), 6),
            0
        );
    }

    // ---- Intrinsics ----

    #[test]
    fn len_trim_basic() {
        assert_eq!(afs_len_trim(b"hello   ".as_ptr(), 8), 5);
        assert_eq!(afs_len_trim(b"   ".as_ptr(), 3), 0);
        assert_eq!(afs_len_trim(b"hello".as_ptr(), 5), 5);
    }

    #[test]
    fn c_strlen_basic() {
        let s = b"scan.f90\0";
        assert_eq!(afs_c_strlen(s.as_ptr()), 8);
    }

    #[test]
    fn adjustl_basic() {
        let mut dest = [0u8; 8];
        afs_adjustl(dest.as_mut_ptr(), b"   hello".as_ptr(), 8);
        assert_eq!(&dest, b"hello   ");
    }

    #[test]
    fn adjustr_basic() {
        let mut dest = [0u8; 8];
        afs_adjustr(dest.as_mut_ptr(), b"hello   ".as_ptr(), 8);
        assert_eq!(&dest, b"   hello");
    }

    #[test]
    fn index_basic() {
        assert_eq!(
            afs_index(b"hello world".as_ptr(), 11, b"world".as_ptr(), 5, 0),
            7
        );
        assert_eq!(
            afs_index(b"hello world".as_ptr(), 11, b"xyz".as_ptr(), 3, 0),
            0
        );
    }

    #[test]
    fn index_back() {
        assert_eq!(afs_index(b"abcabc".as_ptr(), 6, b"abc".as_ptr(), 3, 1), 4);
    }

    #[test]
    fn scan_basic() {
        assert_eq!(afs_scan(b"hello".as_ptr(), 5, b"lo".as_ptr(), 2, 0), 3); // 'l' at pos 3
        assert_eq!(afs_scan(b"hello".as_ptr(), 5, b"xyz".as_ptr(), 3, 0), 0);
    }

    #[test]
    fn verify_basic() {
        assert_eq!(afs_verify(b"aabba".as_ptr(), 5, b"ab".as_ptr(), 2, 0), 0); // all in set
        assert_eq!(afs_verify(b"aabxba".as_ptr(), 6, b"ab".as_ptr(), 2, 0), 4); // 'x' at pos 4
    }

    #[test]
    fn repeat_basic() {
        let mut dest = [0u8; 15];
        afs_repeat(b"abc".as_ptr(), 3, 5, dest.as_mut_ptr());
        assert_eq!(&dest, b"abcabcabcabcabc");
    }

    #[test]
    fn char_ichar() {
        assert_eq!(afs_char(65), b'A');
        assert_eq!(afs_ichar(b'A'), 65);
        let byte = 0xFFu8;
        assert_eq!(afs_ichar_ptr(&byte), 255);
    }

    #[test]
    fn lge_lgt_lle_llt() {
        assert_eq!(afs_lge(b"b".as_ptr(), 1, b"a".as_ptr(), 1), 1);
        assert_eq!(afs_lgt(b"b".as_ptr(), 1, b"a".as_ptr(), 1), 1);
        assert_eq!(afs_lle(b"a".as_ptr(), 1, b"b".as_ptr(), 1), 1);
        assert_eq!(afs_llt(b"a".as_ptr(), 1, b"b".as_ptr(), 1), 1);
        assert_eq!(afs_lge(b"a".as_ptr(), 1, b"a".as_ptr(), 1), 1); // equal → GE true
        assert_eq!(afs_lgt(b"a".as_ptr(), 1, b"a".as_ptr(), 1), 0); // equal → GT false
    }
}
