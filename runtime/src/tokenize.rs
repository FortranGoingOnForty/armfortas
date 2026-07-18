//! Fortran TOKENIZE intrinsic runtime support.

use crate::descriptor::ArrayDescriptor;
use std::ptr;

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
/// allocatables - automatically deallocated on entry - so reusing the
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

fn tokenize_int_kind(kind: i64) -> i64 {
    match kind {
        1 | 2 | 4 | 8 => kind,
        _ => 4,
    }
}

/// TOKENIZE Form 2: `CALL TOKENIZE(STRING, SET, FIRST, LAST)`. FIRST
/// and LAST are allocatable rank-1 integer arrays whose kinds may differ;
/// both are allocated to the token count and filled with the 1-based
/// start/end positions of each token.
#[no_mangle]
pub extern "C" fn afs_tokenize_positions(
    str_ptr: *const u8,
    str_len: i64,
    set_ptr: *const u8,
    set_len: i64,
    first: *mut ArrayDescriptor,
    last: *mut ArrayDescriptor,
    first_kind: i64,
    last_kind: i64,
) {
    if first.is_null() || last.is_null() {
        return;
    }
    let s = tokenize_slice(str_ptr, str_len);
    let set = tokenize_slice(set_ptr, set_len);
    let bounds = tokenize_bounds(s, set);
    let n = bounds.len() as i64;
    let first_kind = tokenize_int_kind(first_kind);
    let last_kind = tokenize_int_kind(last_kind);
    unsafe {
        tokenize_realloc_1d(first, first_kind, n);
        tokenize_realloc_1d(last, last_kind, n);
        let fbase = (*first).base_addr;
        let lbase = (*last).base_addr;
        for (i, &(start, end)) in bounds.iter().enumerate() {
            tokenize_store_int(fbase, i, first_kind, start);
            tokenize_store_int(lbase, i, last_kind, end);
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
                    // in order - exactly the boundaries between tokens.
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

    #[test]
    fn tokenize_bounds_basic() {
        assert_eq!(
            tokenize_bounds(b"a,bb,ccc", b","),
            vec![(1, 1), (3, 4), (6, 8)]
        );
    }

    #[test]
    fn tokenize_bounds_empty_tokens() {
        // ",x,,y," -> "", "x", "", "y", "" with LAST=FIRST-1 for empties.
        assert_eq!(
            tokenize_bounds(b",x,,y,", b","),
            vec![(1, 0), (2, 2), (4, 3), (5, 5), (7, 6)]
        );
    }

    #[test]
    fn tokenize_bounds_edge_cases() {
        assert_eq!(tokenize_bounds(b"", b","), vec![(1, 0)]);
        assert_eq!(tokenize_bounds(b"abc", b""), vec![(1, 3)]);
    }

    #[test]
    fn tokenize_positions_form2() {
        let s = b"a,bb,ccc";
        let mut first = ArrayDescriptor::zeroed();
        let mut last = ArrayDescriptor::zeroed();
        afs_tokenize_positions(s.as_ptr(), 8, b",".as_ptr(), 1, &mut first, &mut last, 4, 4);
        assert_eq!(first.dims[0].upper_bound, 3);
        unsafe {
            let f = std::slice::from_raw_parts(first.base_addr as *const i32, 3);
            let l = std::slice::from_raw_parts(last.base_addr as *const i32, 3);
            assert_eq!(f, &[1, 3, 6]);
            assert_eq!(l, &[1, 4, 8]);
        }
    }

    unsafe fn tokenize_int_values(desc: &ArrayDescriptor, kind: i64, len: usize) -> Vec<i64> {
        (0..len)
            .map(|idx| {
                let p = desc.base_addr.add(idx * kind as usize);
                match kind {
                    1 => *(p as *const i8) as i64,
                    2 => *(p as *const i16) as i64,
                    4 => *(p as *const i32) as i64,
                    8 => *(p as *const i64),
                    _ => unreachable!(),
                }
            })
            .collect()
    }

    #[test]
    fn tokenize_positions_honors_each_result_kind() {
        for first_kind in [1, 2, 4, 8] {
            for last_kind in [1, 2, 4, 8] {
                let mut first = ArrayDescriptor::zeroed();
                let mut last = ArrayDescriptor::zeroed();
                afs_tokenize_positions(
                    b"a,b".as_ptr(),
                    3,
                    b",".as_ptr(),
                    1,
                    &mut first,
                    &mut last,
                    first_kind,
                    last_kind,
                );

                assert_eq!(first.elem_size, first_kind);
                assert_eq!(last.elem_size, last_kind);
                unsafe {
                    assert_eq!(tokenize_int_values(&first, first_kind, 2), [1, 3]);
                    assert_eq!(tokenize_int_values(&last, last_kind, 2), [1, 3]);
                    crate::array::afs_deallocate_array(&mut first, ptr::null_mut());
                    crate::array::afs_deallocate_array(&mut last, ptr::null_mut());
                }
            }
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
            // "a  ", "bb ", "ccc" - each padded to 3.
            assert_eq!(data, b"a  bb ccc");
            let seps = std::slice::from_raw_parts(sep.base_addr, 2);
            assert_eq!(seps, b",,");
        }
    }
}
