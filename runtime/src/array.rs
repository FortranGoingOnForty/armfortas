//! Array memory management — ALLOCATE, DEALLOCATE, allocatable assignment.
//!
//! These functions operate on ArrayDescriptor pointers passed from generated
//! code. They handle allocation, deallocation, reallocation, and descriptor
//! population.

use crate::descriptor::*;
use std::ptr;

// ---- ALLOCATE ----

/// Allocate an array described by the given dimensions.
/// Populates the descriptor with base_addr, elem_size, rank, dims, and flags.
///
/// `dims_ptr` points to `rank` DimDescriptor values (lower, upper, stride=1).
/// `stat` is an optional pointer to a STAT variable (0 = success, nonzero = error).
/// `errmsg` is an optional pointer to a StringDescriptor for error messages.
///
/// If stat is null and allocation fails, the program aborts.
#[no_mangle]
pub extern "C" fn afs_allocate_array(
    desc: *mut ArrayDescriptor,
    elem_size: i64,
    rank: i32,
    dims_ptr: *const DimDescriptor,
    stat: *mut i32,
) {
    if desc.is_null() {
        if !stat.is_null() { unsafe { *stat = 1; } }
        return;
    }

    let desc = unsafe { &mut *desc };

    // Check if already allocated.
    if desc.is_allocated() {
        if !stat.is_null() {
            unsafe { *stat = 2; } // already allocated
            return;
        }
        eprintln!("ALLOCATE: array is already allocated");
        std::process::exit(1);
    }

    // Copy dimensions.
    desc.rank = rank;
    desc.elem_size = elem_size;
    if !dims_ptr.is_null() && rank > 0 {
        let dims_slice = unsafe { std::slice::from_raw_parts(dims_ptr, rank as usize) };
        for (i, dim) in dims_slice.iter().enumerate() {
            desc.dims[i] = *dim;
        }
    }

    // Compute total bytes.
    let total = desc.total_elements();
    let bytes = total * elem_size;

    if bytes <= 0 {
        // Zero-size allocation: valid but produces a null/empty array.
        desc.base_addr = ptr::null_mut();
        desc.flags = DESC_ALLOCATED | DESC_CONTIGUOUS;
        if !stat.is_null() { unsafe { *stat = 0; } }
        return;
    }

    // Allocate.
    let ptr = unsafe { libc_malloc(bytes as usize) };
    if ptr.is_null() {
        if !stat.is_null() {
            unsafe { *stat = 3; } // allocation failed
            return;
        }
        eprintln!("ALLOCATE: out of memory ({} bytes)", bytes);
        std::process::exit(1);
    }

    // Zero-initialize (Fortran doesn't require this, but it's safer).
    unsafe { ptr::write_bytes(ptr, 0, bytes as usize); }

    desc.base_addr = ptr;
    desc.flags = DESC_ALLOCATED | DESC_CONTIGUOUS;

    if !stat.is_null() { unsafe { *stat = 0; } }
}

/// Simplified allocate for a 1D array with given element count.
/// Used by generated code for simple `allocate(a(n))` patterns.
#[no_mangle]
pub extern "C" fn afs_allocate_1d(
    desc: *mut ArrayDescriptor,
    elem_size: i64,
    n: i64,
) {
    let dim = DimDescriptor { lower_bound: 1, upper_bound: n, stride: 1 };
    afs_allocate_array(desc, elem_size, 1, &dim as *const DimDescriptor, ptr::null_mut());
}

// ---- DEALLOCATE ----

/// Deallocate an array, freeing its memory and clearing the descriptor.
///
/// Safe to call on an already-deallocated descriptor (no-op with stat=0).
#[no_mangle]
pub extern "C" fn afs_deallocate_array(
    desc: *mut ArrayDescriptor,
    stat: *mut i32,
) {
    if desc.is_null() {
        if !stat.is_null() { unsafe { *stat = 1; } }
        return;
    }

    let desc = unsafe { &mut *desc };

    if !desc.is_allocated() {
        // Not allocated — not an error with STAT, abort without STAT.
        if !stat.is_null() {
            unsafe { *stat = 0; }
            return;
        }
        // Without STAT, deallocating an unallocated array is an error.
        eprintln!("DEALLOCATE: array is not allocated");
        std::process::exit(1);
    }

    // Free the data.
    if !desc.base_addr.is_null() {
        unsafe { libc_free(desc.base_addr); }
    }

    // Clear the descriptor.
    desc.base_addr = ptr::null_mut();
    desc.flags &= !DESC_ALLOCATED;
    // Leave rank, elem_size, dims intact (they describe the shape for future allocate).

    if !stat.is_null() { unsafe { *stat = 0; } }
}

// ---- ALLOCATABLE ASSIGNMENT ----

/// Assign one array to another with automatic reallocation (F2003).
///
/// If dest's shape doesn't match source's shape, deallocate dest and
/// reallocate with source's shape. Then copy data.
#[no_mangle]
pub extern "C" fn afs_assign_allocatable(
    dest: *mut ArrayDescriptor,
    source: *const ArrayDescriptor,
) {
    if dest.is_null() || source.is_null() { return; }

    let dest = unsafe { &mut *dest };
    let source = unsafe { &*source };

    // Check if shapes match.
    let shapes_match = dest.rank == source.rank && {
        (0..dest.rank as usize).all(|i| {
            dest.dims[i].extent() == source.dims[i].extent()
        })
    };

    if !shapes_match || !dest.is_allocated() {
        // Deallocate dest if allocated.
        if dest.is_allocated() && !dest.base_addr.is_null() {
            unsafe { libc_free(dest.base_addr); }
            dest.base_addr = ptr::null_mut();
            dest.flags &= !DESC_ALLOCATED;
        }

        // Allocate with source's shape.
        dest.rank = source.rank;
        dest.elem_size = source.elem_size;
        for i in 0..source.rank as usize {
            dest.dims[i] = source.dims[i];
            dest.dims[i].stride = 1; // dest is always contiguous
        }

        let bytes = dest.total_bytes();
        if bytes > 0 {
            let ptr = unsafe { libc_malloc(bytes as usize) };
            if ptr.is_null() {
                eprintln!("ALLOCATE (assignment): out of memory ({} bytes)", bytes);
                std::process::exit(1);
            }
            dest.base_addr = ptr;
        }
        dest.flags = DESC_ALLOCATED | DESC_CONTIGUOUS;
    }

    // Copy data. Use ptr::copy (not copy_nonoverlapping) to handle self-assignment.
    let bytes = source.total_bytes();
    if bytes > 0 && !source.base_addr.is_null() && !dest.base_addr.is_null() {
        unsafe {
            ptr::copy(source.base_addr, dest.base_addr, bytes as usize);
        }
    }
}

// ---- MOVE_ALLOC ----

/// Transfer allocation from `from` to `to` (F2003 MOVE_ALLOC).
///
/// `to` is deallocated if allocated, then receives `from`'s descriptor.
/// `from` is cleared (becomes unallocated).
#[no_mangle]
pub extern "C" fn afs_move_alloc(
    from: *mut ArrayDescriptor,
    to: *mut ArrayDescriptor,
) {
    if from.is_null() || to.is_null() { return; }

    let from_desc = unsafe { &mut *from };
    let to_desc = unsafe { &mut *to };

    // Deallocate `to` if allocated.
    if to_desc.is_allocated() && !to_desc.base_addr.is_null() {
        unsafe { libc_free(to_desc.base_addr); }
    }

    // Copy descriptor from `from` to `to`.
    *to_desc = from_desc.clone();

    // Clear `from`.
    from_desc.base_addr = ptr::null_mut();
    from_desc.flags &= !DESC_ALLOCATED;
}

// ---- ALLOCATED INTRINSIC ----

/// Check if an array is allocated. Returns 1 (true) or 0 (false).
#[no_mangle]
pub extern "C" fn afs_allocated(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() { return 0; }
    unsafe { (*desc).is_allocated() as i32 }
}

// ---- ARRAY SECTIONS ----

/// Create a section descriptor that views into an existing array.
///
/// `specs` is an array of SectionSpec values (one per dimension), specifying
/// the start, end, and stride of the section. The result descriptor points
/// into the source's data with adjusted base_addr and strides.
#[repr(C)]
#[derive(Clone, Copy)]
pub struct SectionSpec {
    pub start: i64,
    pub end: i64,
    pub stride: i64,
}

#[no_mangle]
pub extern "C" fn afs_create_section(
    source: *const ArrayDescriptor,
    result: *mut ArrayDescriptor,
    specs: *const SectionSpec,
    n_dims: i32,
) {
    if source.is_null() || result.is_null() || specs.is_null() { return; }

    let source = unsafe { &*source };
    let result = unsafe { &mut *result };
    let specs_slice = unsafe { std::slice::from_raw_parts(specs, n_dims as usize) };

    result.elem_size = source.elem_size;
    result.rank = n_dims;
    result.flags = DESC_CONTIGUOUS; // sections may not be contiguous
    // Don't set DESC_ALLOCATED — section doesn't own the data.

    // Compute base address offset and new dims.
    let mut byte_offset: i64 = 0;
    let mut source_multiplier: i64 = 1;

    for (i, spec) in specs_slice.iter().enumerate() {
        let src_dim = &source.dims[i];

        // Offset from source lower bound to section start.
        let start_idx = spec.start - src_dim.lower_bound;
        byte_offset += start_idx * source_multiplier * src_dim.stride * source.elem_size;

        // New dimension bounds. Extent = max(0, (end - start) / stride + 1).
        // For negative strides, start > end and (end-start)/stride is positive.
        // For a positive stride where start > end, result is empty (extent 0).
        let extent = if spec.stride == 0 {
            1
        } else if (spec.stride > 0 && spec.start > spec.end)
            || (spec.stride < 0 && spec.start < spec.end) {
            0 // empty section
        } else {
            (spec.end - spec.start) / spec.stride + 1
        };
        result.dims[i] = DimDescriptor {
            lower_bound: 1, // sections are always 1-based
            upper_bound: extent,
            stride: src_dim.stride * spec.stride,
        };

        source_multiplier *= src_dim.extent();
    }

    // Result base_addr = source base_addr + offset.
    if !source.base_addr.is_null() {
        // byte_offset can be negative for negative-stride sections.
        result.base_addr = unsafe { source.base_addr.offset(byte_offset as isize) };
    } else {
        result.base_addr = ptr::null_mut();
    }

    // Check contiguity.
    let is_contig = (0..n_dims as usize).all(|i| result.dims[i].stride == 1);
    if !is_contig {
        result.flags &= !DESC_CONTIGUOUS;
    }
}

// ---- libc interop ----

extern "C" {
    fn malloc(size: usize) -> *mut u8;
    fn free(ptr: *mut u8);
}

unsafe fn libc_malloc(size: usize) -> *mut u8 {
    malloc(size)
}

unsafe fn libc_free(ptr: *mut u8) {
    free(ptr)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn allocate_1d() {
        let mut desc = ArrayDescriptor::zeroed();
        afs_allocate_1d(&mut desc, 4, 10);
        assert!(desc.is_allocated());
        assert_eq!(desc.rank, 1);
        assert_eq!(desc.elem_size, 4);
        assert_eq!(desc.total_elements(), 10);
        assert!(!desc.base_addr.is_null());
        afs_deallocate_array(&mut desc, ptr::null_mut());
        assert!(!desc.is_allocated());
        assert!(desc.base_addr.is_null());
    }

    #[test]
    fn allocate_2d() {
        let mut desc = ArrayDescriptor::zeroed();
        let dims = [
            DimDescriptor { lower_bound: 1, upper_bound: 3, stride: 1 },
            DimDescriptor { lower_bound: 1, upper_bound: 4, stride: 1 },
        ];
        afs_allocate_array(&mut desc, 8, 2, dims.as_ptr(), ptr::null_mut());
        assert!(desc.is_allocated());
        assert_eq!(desc.total_elements(), 12);
        assert_eq!(desc.total_bytes(), 96); // 12 * 8
        afs_deallocate_array(&mut desc, ptr::null_mut());
    }

    #[test]
    fn allocate_with_stat() {
        let mut desc = ArrayDescriptor::zeroed();
        let mut stat: i32 = -1;
        afs_allocate_1d(&mut desc, 4, 10);
        // Allocating again should fail with stat.
        let dim = DimDescriptor { lower_bound: 1, upper_bound: 10, stride: 1 };
        afs_allocate_array(&mut desc, 4, 1, &dim, &mut stat);
        assert_eq!(stat, 2); // already allocated
        afs_deallocate_array(&mut desc, ptr::null_mut());
    }

    #[test]
    fn move_alloc() {
        let mut from = ArrayDescriptor::zeroed();
        let mut to = ArrayDescriptor::zeroed();
        afs_allocate_1d(&mut from, 4, 100);
        assert!(from.is_allocated());
        assert!(!to.is_allocated());

        afs_move_alloc(&mut from, &mut to);
        assert!(!from.is_allocated());
        assert!(to.is_allocated());
        assert_eq!(to.total_elements(), 100);

        afs_deallocate_array(&mut to, ptr::null_mut());
    }

    #[test]
    fn allocated_intrinsic() {
        let mut desc = ArrayDescriptor::zeroed();
        assert_eq!(afs_allocated(&desc), 0);
        afs_allocate_1d(&mut desc, 4, 10);
        assert_eq!(afs_allocated(&desc), 1);
        afs_deallocate_array(&mut desc, ptr::null_mut());
        assert_eq!(afs_allocated(&desc), 0);
    }

    #[test]
    fn assign_allocatable() {
        let mut source = ArrayDescriptor::zeroed();
        let mut dest = ArrayDescriptor::zeroed();

        afs_allocate_1d(&mut source, 4, 5);
        // Write some data into source.
        unsafe {
            let data = source.base_addr as *mut i32;
            for i in 0..5 {
                *data.add(i) = (i + 1) as i32;
            }
        }

        afs_assign_allocatable(&mut dest, &source);
        assert!(dest.is_allocated());
        assert_eq!(dest.total_elements(), 5);

        // Verify data was copied.
        unsafe {
            let data = dest.base_addr as *const i32;
            for i in 0..5 {
                assert_eq!(*data.add(i), (i + 1) as i32);
            }
        }

        afs_deallocate_array(&mut source, ptr::null_mut());
        afs_deallocate_array(&mut dest, ptr::null_mut());
    }

    #[test]
    fn zero_size_allocation() {
        let mut desc = ArrayDescriptor::zeroed();
        afs_allocate_1d(&mut desc, 4, 0);
        assert!(desc.is_allocated());
        assert_eq!(desc.total_elements(), 0);
        afs_deallocate_array(&mut desc, ptr::null_mut());
    }
}

// ---- Array query intrinsics ----

/// SIZE(array) — total number of elements.
#[no_mangle]
pub extern "C" fn afs_array_size(desc: *const ArrayDescriptor) -> i64 {
    if desc.is_null() { return 0; }
    unsafe { (*desc).total_elements() }
}

/// SIZE(array, dim) — number of elements along dimension `dim` (1-based).
#[no_mangle]
pub extern "C" fn afs_array_size_dim(desc: *const ArrayDescriptor, dim: i32) -> i64 {
    if desc.is_null() || dim < 1 { return 0; }
    let d = unsafe { &*desc };
    let idx = (dim - 1) as usize;
    if idx < d.rank as usize {
        d.dims[idx].extent()
    } else { 0 }
}

/// LBOUND(array, dim) — lower bound along dimension `dim` (1-based).
#[no_mangle]
pub extern "C" fn afs_array_lbound(desc: *const ArrayDescriptor, dim: i32) -> i64 {
    if desc.is_null() || dim < 1 { return 1; }
    let d = unsafe { &*desc };
    let idx = (dim - 1) as usize;
    if idx < d.rank as usize { d.dims[idx].lower_bound } else { 1 }
}

/// UBOUND(array, dim) — upper bound along dimension `dim` (1-based).
#[no_mangle]
pub extern "C" fn afs_array_ubound(desc: *const ArrayDescriptor, dim: i32) -> i64 {
    if desc.is_null() || dim < 1 { return 0; }
    let d = unsafe { &*desc };
    let idx = (dim - 1) as usize;
    if idx < d.rank as usize { d.dims[idx].upper_bound } else { 0 }
}

/// ALLOCATED(array) — check if array is allocated (returns 1 or 0).
#[no_mangle]
pub extern "C" fn afs_array_allocated(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() { return 0; }
    unsafe { (*desc).is_allocated() as i32 }
}

/// SUM(array) — sum all elements (real(8) version).
/// Respects strides for non-contiguous sections.
#[no_mangle]
pub extern "C" fn afs_array_sum_real8(desc: *const ArrayDescriptor) -> f64 {
    if desc.is_null() { return 0.0; }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() { return 0.0; }
    let n = d.total_elements() as usize;
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const f64;
    let mut sum = 0.0;
    for i in 0..n {
        sum += unsafe { *ptr.add(i * stride) };
    }
    sum
}

/// SUM(array) — sum all elements (integer(4) version).
/// Respects strides for non-contiguous sections.
#[no_mangle]
pub extern "C" fn afs_array_sum_int(desc: *const ArrayDescriptor) -> i64 {
    if desc.is_null() { return 0; }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() { return 0; }
    let n = d.total_elements() as usize;
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const i32;
    let mut sum: i64 = 0;
    for i in 0..n {
        sum += unsafe { *ptr.add(i * stride) } as i64;
    }
    sum
}

/// PRODUCT(array) — product of all elements (real(8) version).
#[no_mangle]
pub extern "C" fn afs_array_product_real8(desc: *const ArrayDescriptor) -> f64 {
    if desc.is_null() { return 1.0; }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() { return 1.0; }
    let n = d.total_elements() as usize;
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const f64;
    let mut prod = 1.0;
    for i in 0..n {
        prod *= unsafe { *ptr.add(i * stride) };
    }
    prod
}

/// PRODUCT(array) — product of all elements (integer(4) version).
#[no_mangle]
pub extern "C" fn afs_array_product_int(desc: *const ArrayDescriptor) -> i64 {
    if desc.is_null() { return 1; }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() { return 1; }
    let n = d.total_elements() as usize;
    if n == 0 { return 1; }
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const i32;
    let mut prod: i64 = 1;
    for i in 0..n {
        prod *= unsafe { *ptr.add(i * stride) } as i64;
    }
    prod
}

/// MAXVAL(array) — maximum element (real(8) version). Respects strides.
#[no_mangle]
pub extern "C" fn afs_array_maxval_real8(desc: *const ArrayDescriptor) -> f64 {
    if desc.is_null() { return f64::NEG_INFINITY; }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() { return f64::NEG_INFINITY; }
    let n = d.total_elements() as usize;
    if n == 0 { return f64::NEG_INFINITY; }
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const f64;
    let mut max = unsafe { *ptr };
    for i in 1..n {
        let v = unsafe { *ptr.add(i * stride) };
        if v > max { max = v; }
    }
    max
}

/// MINVAL(array) — minimum element (real(8) version). Respects strides.
#[no_mangle]
pub extern "C" fn afs_array_minval_real8(desc: *const ArrayDescriptor) -> f64 {
    if desc.is_null() { return f64::INFINITY; }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() { return f64::INFINITY; }
    let n = d.total_elements() as usize;
    if n == 0 { return f64::INFINITY; }
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const f64;
    let mut min = unsafe { *ptr };
    for i in 1..n {
        let v = unsafe { *ptr.add(i * stride) };
        if v < min { min = v; }
    }
    min
}

/// MAXVAL(array) — maximum element (integer(4) version). Respects strides.
#[no_mangle]
pub extern "C" fn afs_array_maxval_int(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() { return i32::MIN; }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() { return i32::MIN; }
    let n = d.total_elements() as usize;
    if n == 0 { return i32::MIN; }
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const i32;
    let mut max = unsafe { *ptr };
    for i in 1..n {
        let v = unsafe { *ptr.add(i * stride) };
        if v > max { max = v; }
    }
    max
}

/// MINVAL(array) — minimum element (integer(4) version). Respects strides.
#[no_mangle]
pub extern "C" fn afs_array_minval_int(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() { return i32::MAX; }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() { return i32::MAX; }
    let n = d.total_elements() as usize;
    if n == 0 { return i32::MAX; }
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const i32;
    let mut min = unsafe { *ptr };
    for i in 1..n {
        let v = unsafe { *ptr.add(i * stride) };
        if v < min { min = v; }
    }
    min
}

/// TRANSPOSE(source, result) — matrix transpose (real(8) version).
/// source is (m x n), result is (n x m). Both descriptors must be allocated.
#[no_mangle]
pub extern "C" fn afs_transpose_real8(
    source: *const ArrayDescriptor,
    result: *mut ArrayDescriptor,
) {
    if source.is_null() || result.is_null() { return; }
    let src = unsafe { &*source };
    if src.rank < 2 || src.base_addr.is_null() { return; }

    let m = src.dims[0].extent() as usize;
    let n = src.dims[1].extent() as usize;
    let sp = src.base_addr as *const f64;

    // Allocate result as (n x m).
    afs_allocate_1d(result, 8, (n * m) as i64);
    let res = unsafe { &mut *result };
    res.rank = 2;
    res.dims[0] = DimDescriptor { lower_bound: 1, upper_bound: n as i64, stride: 1 };
    res.dims[1] = DimDescriptor { lower_bound: 1, upper_bound: m as i64, stride: 1 };
    let rp = res.base_addr as *mut f64;

    for i in 0..m {
        for j in 0..n {
            unsafe { *rp.add(j * m + i) = *sp.add(i * n + j); }
        }
    }
}

/// MATMUL(a, b, result) — matrix multiplication (real(8) version).
/// a is (m x k), b is (k x n), result is (m x n).
#[no_mangle]
pub extern "C" fn afs_matmul_real8(
    a: *const ArrayDescriptor,
    b: *const ArrayDescriptor,
    result: *mut ArrayDescriptor,
) {
    if a.is_null() || b.is_null() || result.is_null() { return; }
    let da = unsafe { &*a };
    let db = unsafe { &*b };
    if da.base_addr.is_null() || db.base_addr.is_null() { return; }

    let m = da.dims[0].extent() as usize;
    let k = if da.rank >= 2 { da.dims[1].extent() as usize } else { 1 };
    let n = if db.rank >= 2 { db.dims[1].extent() as usize } else { db.dims[0].extent() as usize };

    // For vector * matrix or matrix * vector, adjust dimensions.
    let ap = da.base_addr as *const f64;
    let bp = db.base_addr as *const f64;

    // Allocate result.
    afs_allocate_1d(result, 8, (m * n) as i64);
    let res = unsafe { &mut *result };
    res.rank = 2;
    res.dims[0] = DimDescriptor { lower_bound: 1, upper_bound: m as i64, stride: 1 };
    res.dims[1] = DimDescriptor { lower_bound: 1, upper_bound: n as i64, stride: 1 };
    let rp = res.base_addr as *mut f64;

    // Triple loop: C(i,j) = sum_l A(i,l) * B(l,j)
    for i in 0..m {
        for j in 0..n {
            let mut sum = 0.0;
            for l in 0..k {
                let a_val = unsafe { *ap.add(i * k + l) };
                let b_val = unsafe { *bp.add(l * n + j) };
                sum += a_val * b_val;
            }
            unsafe { *rp.add(i * n + j) = sum; }
        }
    }
}

/// DOT_PRODUCT(a, b) — vector dot product (real(8) version).
/// Respects strides for non-contiguous array sections.
#[no_mangle]
pub extern "C" fn afs_dot_product_real8(
    a: *const ArrayDescriptor,
    b: *const ArrayDescriptor,
) -> f64 {
    if a.is_null() || b.is_null() { return 0.0; }
    let da = unsafe { &*a };
    let db = unsafe { &*b };
    if da.base_addr.is_null() || db.base_addr.is_null() { return 0.0; }
    let n = da.dims[0].extent().min(db.dims[0].extent()) as usize;
    let stride_a = da.dims[0].stride as usize;
    let stride_b = db.dims[0].stride as usize;
    let pa = da.base_addr as *const f64;
    let pb = db.base_addr as *const f64;
    let mut dot = 0.0;
    for i in 0..n {
        dot += unsafe { *pa.add(i * stride_a) * *pb.add(i * stride_b) };
    }
    dot
}

