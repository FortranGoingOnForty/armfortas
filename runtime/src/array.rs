//! Array memory management — ALLOCATE, DEALLOCATE, allocatable assignment.
//!
//! These functions operate on ArrayDescriptor pointers passed from generated
//! code. They handle allocation, deallocation, reallocation, and descriptor
//! population.

use crate::descriptor::*;
use std::ptr;

// ---- BOUNDS CHECKS ----

/// Abort if an array subscript is outside the legal closed interval.
#[no_mangle]
pub extern "C" fn afs_check_bounds(index: i64, lower: i64, upper: i64) {
    if index < lower || index > upper {
        eprintln!(
            "Bounds check failed: index {} outside [{}, {}]",
            index, lower, upper
        );
        std::process::exit(1);
    }
}

fn bulk_len(n: i64) -> usize {
    if n <= 0 {
        0
    } else {
        usize::try_from(n).unwrap_or(0)
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn fill_i32_impl(dest: *mut i32, len: usize, value: i32) {
    use core::arch::aarch64::{vdupq_n_s32, vst1q_s32};

    let mut i = 0usize;
    let splat = vdupq_n_s32(value);
    while i + 4 <= len {
        vst1q_s32(dest.add(i), splat);
        i += 4;
    }
    while i < len {
        *dest.add(i) = value;
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn fill_i32_impl(dest: *mut i32, len: usize, value: i32) {
    for i in 0..len {
        *dest.add(i) = value;
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn fill_f32_impl(dest: *mut f32, len: usize, value: f32) {
    use core::arch::aarch64::{vdupq_n_f32, vst1q_f32};

    let mut i = 0usize;
    let splat = vdupq_n_f32(value);
    while i + 4 <= len {
        vst1q_f32(dest.add(i), splat);
        i += 4;
    }
    while i < len {
        *dest.add(i) = value;
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn fill_f32_impl(dest: *mut f32, len: usize, value: f32) {
    for i in 0..len {
        *dest.add(i) = value;
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn fill_f64_impl(dest: *mut f64, len: usize, value: f64) {
    use core::arch::aarch64::{vdupq_n_f64, vst1q_f64};

    let mut i = 0usize;
    let splat = vdupq_n_f64(value);
    while i + 2 <= len {
        vst1q_f64(dest.add(i), splat);
        i += 2;
    }
    while i < len {
        *dest.add(i) = value;
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn fill_f64_impl(dest: *mut f64, len: usize, value: f64) {
    for i in 0..len {
        *dest.add(i) = value;
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn add_i32_impl(dest: *mut i32, lhs: *const i32, rhs: *const i32, len: usize) {
    use core::arch::aarch64::{vaddq_s32, vld1q_s32, vst1q_s32};

    let mut i = 0usize;
    while i + 4 <= len {
        let a = vld1q_s32(lhs.add(i));
        let b = vld1q_s32(rhs.add(i));
        vst1q_s32(dest.add(i), vaddq_s32(a, b));
        i += 4;
    }
    while i < len {
        *dest.add(i) = *lhs.add(i) + *rhs.add(i);
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn add_i32_impl(dest: *mut i32, lhs: *const i32, rhs: *const i32, len: usize) {
    for i in 0..len {
        *dest.add(i) = *lhs.add(i) + *rhs.add(i);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn add_f32_impl(dest: *mut f32, lhs: *const f32, rhs: *const f32, len: usize) {
    use core::arch::aarch64::{vaddq_f32, vld1q_f32, vst1q_f32};

    let mut i = 0usize;
    while i + 4 <= len {
        let a = vld1q_f32(lhs.add(i));
        let b = vld1q_f32(rhs.add(i));
        vst1q_f32(dest.add(i), vaddq_f32(a, b));
        i += 4;
    }
    while i < len {
        *dest.add(i) = *lhs.add(i) + *rhs.add(i);
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn add_f32_impl(dest: *mut f32, lhs: *const f32, rhs: *const f32, len: usize) {
    for i in 0..len {
        *dest.add(i) = *lhs.add(i) + *rhs.add(i);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn add_f64_impl(dest: *mut f64, lhs: *const f64, rhs: *const f64, len: usize) {
    use core::arch::aarch64::{vaddq_f64, vld1q_f64, vst1q_f64};

    let mut i = 0usize;
    while i + 2 <= len {
        let a = vld1q_f64(lhs.add(i));
        let b = vld1q_f64(rhs.add(i));
        vst1q_f64(dest.add(i), vaddq_f64(a, b));
        i += 2;
    }
    while i < len {
        *dest.add(i) = *lhs.add(i) + *rhs.add(i);
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn add_f64_impl(dest: *mut f64, lhs: *const f64, rhs: *const f64, len: usize) {
    for i in 0..len {
        *dest.add(i) = *lhs.add(i) + *rhs.add(i);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn sub_i32_impl(dest: *mut i32, lhs: *const i32, rhs: *const i32, len: usize) {
    use core::arch::aarch64::{vld1q_s32, vst1q_s32, vsubq_s32};

    let mut i = 0usize;
    while i + 4 <= len {
        let a = vld1q_s32(lhs.add(i));
        let b = vld1q_s32(rhs.add(i));
        vst1q_s32(dest.add(i), vsubq_s32(a, b));
        i += 4;
    }
    while i < len {
        *dest.add(i) = *lhs.add(i) - *rhs.add(i);
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn sub_i32_impl(dest: *mut i32, lhs: *const i32, rhs: *const i32, len: usize) {
    for i in 0..len {
        *dest.add(i) = *lhs.add(i) - *rhs.add(i);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn sub_f32_impl(dest: *mut f32, lhs: *const f32, rhs: *const f32, len: usize) {
    use core::arch::aarch64::{vld1q_f32, vst1q_f32, vsubq_f32};

    let mut i = 0usize;
    while i + 4 <= len {
        let a = vld1q_f32(lhs.add(i));
        let b = vld1q_f32(rhs.add(i));
        vst1q_f32(dest.add(i), vsubq_f32(a, b));
        i += 4;
    }
    while i < len {
        *dest.add(i) = *lhs.add(i) - *rhs.add(i);
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn sub_f32_impl(dest: *mut f32, lhs: *const f32, rhs: *const f32, len: usize) {
    for i in 0..len {
        *dest.add(i) = *lhs.add(i) - *rhs.add(i);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn sub_f64_impl(dest: *mut f64, lhs: *const f64, rhs: *const f64, len: usize) {
    use core::arch::aarch64::{vld1q_f64, vst1q_f64, vsubq_f64};

    let mut i = 0usize;
    while i + 2 <= len {
        let a = vld1q_f64(lhs.add(i));
        let b = vld1q_f64(rhs.add(i));
        vst1q_f64(dest.add(i), vsubq_f64(a, b));
        i += 2;
    }
    while i < len {
        *dest.add(i) = *lhs.add(i) - *rhs.add(i);
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn sub_f64_impl(dest: *mut f64, lhs: *const f64, rhs: *const f64, len: usize) {
    for i in 0..len {
        *dest.add(i) = *lhs.add(i) - *rhs.add(i);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn mul_i32_impl(dest: *mut i32, lhs: *const i32, rhs: *const i32, len: usize) {
    use core::arch::aarch64::{vld1q_s32, vmulq_s32, vst1q_s32};

    let mut i = 0usize;
    while i + 4 <= len {
        let a = vld1q_s32(lhs.add(i));
        let b = vld1q_s32(rhs.add(i));
        vst1q_s32(dest.add(i), vmulq_s32(a, b));
        i += 4;
    }
    while i < len {
        *dest.add(i) = *lhs.add(i) * *rhs.add(i);
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn mul_i32_impl(dest: *mut i32, lhs: *const i32, rhs: *const i32, len: usize) {
    for i in 0..len {
        *dest.add(i) = *lhs.add(i) * *rhs.add(i);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn mul_f32_impl(dest: *mut f32, lhs: *const f32, rhs: *const f32, len: usize) {
    use core::arch::aarch64::{vld1q_f32, vmulq_f32, vst1q_f32};

    let mut i = 0usize;
    while i + 4 <= len {
        let a = vld1q_f32(lhs.add(i));
        let b = vld1q_f32(rhs.add(i));
        vst1q_f32(dest.add(i), vmulq_f32(a, b));
        i += 4;
    }
    while i < len {
        *dest.add(i) = *lhs.add(i) * *rhs.add(i);
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn mul_f32_impl(dest: *mut f32, lhs: *const f32, rhs: *const f32, len: usize) {
    for i in 0..len {
        *dest.add(i) = *lhs.add(i) * *rhs.add(i);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn mul_f64_impl(dest: *mut f64, lhs: *const f64, rhs: *const f64, len: usize) {
    use core::arch::aarch64::{vld1q_f64, vmulq_f64, vst1q_f64};

    let mut i = 0usize;
    while i + 2 <= len {
        let a = vld1q_f64(lhs.add(i));
        let b = vld1q_f64(rhs.add(i));
        vst1q_f64(dest.add(i), vmulq_f64(a, b));
        i += 2;
    }
    while i < len {
        *dest.add(i) = *lhs.add(i) * *rhs.add(i);
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn mul_f64_impl(dest: *mut f64, lhs: *const f64, rhs: *const f64, len: usize) {
    for i in 0..len {
        *dest.add(i) = *lhs.add(i) * *rhs.add(i);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn add_scalar_i32_impl(dest: *mut i32, src: *const i32, scalar: i32, len: usize) {
    use core::arch::aarch64::{vaddq_s32, vdupq_n_s32, vld1q_s32, vst1q_s32};

    let mut i = 0usize;
    let splat = vdupq_n_s32(scalar);
    while i + 4 <= len {
        let a = vld1q_s32(src.add(i));
        vst1q_s32(dest.add(i), vaddq_s32(a, splat));
        i += 4;
    }
    while i < len {
        *dest.add(i) = *src.add(i) + scalar;
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn add_scalar_i32_impl(dest: *mut i32, src: *const i32, scalar: i32, len: usize) {
    for i in 0..len {
        *dest.add(i) = *src.add(i) + scalar;
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn add_scalar_f32_impl(dest: *mut f32, src: *const f32, scalar: f32, len: usize) {
    use core::arch::aarch64::{vaddq_f32, vdupq_n_f32, vld1q_f32, vst1q_f32};

    let mut i = 0usize;
    let splat = vdupq_n_f32(scalar);
    while i + 4 <= len {
        let a = vld1q_f32(src.add(i));
        vst1q_f32(dest.add(i), vaddq_f32(a, splat));
        i += 4;
    }
    while i < len {
        *dest.add(i) = *src.add(i) + scalar;
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn add_scalar_f32_impl(dest: *mut f32, src: *const f32, scalar: f32, len: usize) {
    for i in 0..len {
        *dest.add(i) = *src.add(i) + scalar;
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn add_scalar_f64_impl(dest: *mut f64, src: *const f64, scalar: f64, len: usize) {
    use core::arch::aarch64::{vaddq_f64, vdupq_n_f64, vld1q_f64, vst1q_f64};

    let mut i = 0usize;
    let splat = vdupq_n_f64(scalar);
    while i + 2 <= len {
        let a = vld1q_f64(src.add(i));
        vst1q_f64(dest.add(i), vaddq_f64(a, splat));
        i += 2;
    }
    while i < len {
        *dest.add(i) = *src.add(i) + scalar;
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn add_scalar_f64_impl(dest: *mut f64, src: *const f64, scalar: f64, len: usize) {
    for i in 0..len {
        *dest.add(i) = *src.add(i) + scalar;
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn sub_scalar_i32_impl(dest: *mut i32, src: *const i32, scalar: i32, len: usize) {
    use core::arch::aarch64::{vdupq_n_s32, vld1q_s32, vst1q_s32, vsubq_s32};

    let mut i = 0usize;
    let splat = vdupq_n_s32(scalar);
    while i + 4 <= len {
        let a = vld1q_s32(src.add(i));
        vst1q_s32(dest.add(i), vsubq_s32(a, splat));
        i += 4;
    }
    while i < len {
        *dest.add(i) = *src.add(i) - scalar;
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn sub_scalar_i32_impl(dest: *mut i32, src: *const i32, scalar: i32, len: usize) {
    for i in 0..len {
        *dest.add(i) = *src.add(i) - scalar;
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn sub_scalar_f32_impl(dest: *mut f32, src: *const f32, scalar: f32, len: usize) {
    use core::arch::aarch64::{vdupq_n_f32, vld1q_f32, vst1q_f32, vsubq_f32};

    let mut i = 0usize;
    let splat = vdupq_n_f32(scalar);
    while i + 4 <= len {
        let a = vld1q_f32(src.add(i));
        vst1q_f32(dest.add(i), vsubq_f32(a, splat));
        i += 4;
    }
    while i < len {
        *dest.add(i) = *src.add(i) - scalar;
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn sub_scalar_f32_impl(dest: *mut f32, src: *const f32, scalar: f32, len: usize) {
    for i in 0..len {
        *dest.add(i) = *src.add(i) - scalar;
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn sub_scalar_f64_impl(dest: *mut f64, src: *const f64, scalar: f64, len: usize) {
    use core::arch::aarch64::{vdupq_n_f64, vld1q_f64, vst1q_f64, vsubq_f64};

    let mut i = 0usize;
    let splat = vdupq_n_f64(scalar);
    while i + 2 <= len {
        let a = vld1q_f64(src.add(i));
        vst1q_f64(dest.add(i), vsubq_f64(a, splat));
        i += 2;
    }
    while i < len {
        *dest.add(i) = *src.add(i) - scalar;
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn sub_scalar_f64_impl(dest: *mut f64, src: *const f64, scalar: f64, len: usize) {
    for i in 0..len {
        *dest.add(i) = *src.add(i) - scalar;
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn scalar_sub_i32_impl(dest: *mut i32, scalar: i32, src: *const i32, len: usize) {
    use core::arch::aarch64::{vdupq_n_s32, vld1q_s32, vst1q_s32, vsubq_s32};

    let mut i = 0usize;
    let splat = vdupq_n_s32(scalar);
    while i + 4 <= len {
        let a = vld1q_s32(src.add(i));
        vst1q_s32(dest.add(i), vsubq_s32(splat, a));
        i += 4;
    }
    while i < len {
        *dest.add(i) = scalar - *src.add(i);
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn scalar_sub_i32_impl(dest: *mut i32, scalar: i32, src: *const i32, len: usize) {
    for i in 0..len {
        *dest.add(i) = scalar - *src.add(i);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn scalar_sub_f32_impl(dest: *mut f32, scalar: f32, src: *const f32, len: usize) {
    use core::arch::aarch64::{vdupq_n_f32, vld1q_f32, vst1q_f32, vsubq_f32};

    let mut i = 0usize;
    let splat = vdupq_n_f32(scalar);
    while i + 4 <= len {
        let a = vld1q_f32(src.add(i));
        vst1q_f32(dest.add(i), vsubq_f32(splat, a));
        i += 4;
    }
    while i < len {
        *dest.add(i) = scalar - *src.add(i);
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn scalar_sub_f32_impl(dest: *mut f32, scalar: f32, src: *const f32, len: usize) {
    for i in 0..len {
        *dest.add(i) = scalar - *src.add(i);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn scalar_sub_f64_impl(dest: *mut f64, scalar: f64, src: *const f64, len: usize) {
    use core::arch::aarch64::{vdupq_n_f64, vld1q_f64, vst1q_f64, vsubq_f64};

    let mut i = 0usize;
    let splat = vdupq_n_f64(scalar);
    while i + 2 <= len {
        let a = vld1q_f64(src.add(i));
        vst1q_f64(dest.add(i), vsubq_f64(splat, a));
        i += 2;
    }
    while i < len {
        *dest.add(i) = scalar - *src.add(i);
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn scalar_sub_f64_impl(dest: *mut f64, scalar: f64, src: *const f64, len: usize) {
    for i in 0..len {
        *dest.add(i) = scalar - *src.add(i);
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn mul_scalar_i32_impl(dest: *mut i32, src: *const i32, scalar: i32, len: usize) {
    use core::arch::aarch64::{vdupq_n_s32, vld1q_s32, vmulq_s32, vst1q_s32};

    let mut i = 0usize;
    let splat = vdupq_n_s32(scalar);
    while i + 4 <= len {
        let a = vld1q_s32(src.add(i));
        vst1q_s32(dest.add(i), vmulq_s32(a, splat));
        i += 4;
    }
    while i < len {
        *dest.add(i) = *src.add(i) * scalar;
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn mul_scalar_i32_impl(dest: *mut i32, src: *const i32, scalar: i32, len: usize) {
    for i in 0..len {
        *dest.add(i) = *src.add(i) * scalar;
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn mul_scalar_f32_impl(dest: *mut f32, src: *const f32, scalar: f32, len: usize) {
    use core::arch::aarch64::{vdupq_n_f32, vld1q_f32, vmulq_f32, vst1q_f32};

    let mut i = 0usize;
    let splat = vdupq_n_f32(scalar);
    while i + 4 <= len {
        let a = vld1q_f32(src.add(i));
        vst1q_f32(dest.add(i), vmulq_f32(a, splat));
        i += 4;
    }
    while i < len {
        *dest.add(i) = *src.add(i) * scalar;
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn mul_scalar_f32_impl(dest: *mut f32, src: *const f32, scalar: f32, len: usize) {
    for i in 0..len {
        *dest.add(i) = *src.add(i) * scalar;
    }
}

#[cfg(target_arch = "aarch64")]
unsafe fn mul_scalar_f64_impl(dest: *mut f64, src: *const f64, scalar: f64, len: usize) {
    use core::arch::aarch64::{vdupq_n_f64, vld1q_f64, vmulq_f64, vst1q_f64};

    let mut i = 0usize;
    let splat = vdupq_n_f64(scalar);
    while i + 2 <= len {
        let a = vld1q_f64(src.add(i));
        vst1q_f64(dest.add(i), vmulq_f64(a, splat));
        i += 2;
    }
    while i < len {
        *dest.add(i) = *src.add(i) * scalar;
        i += 1;
    }
}

#[cfg(not(target_arch = "aarch64"))]
unsafe fn mul_scalar_f64_impl(dest: *mut f64, src: *const f64, scalar: f64, len: usize) {
    for i in 0..len {
        *dest.add(i) = *src.add(i) * scalar;
    }
}

#[no_mangle]
pub extern "C" fn afs_fill_i32(dest: *mut i32, n: i64, value: i32) {
    if dest.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        fill_i32_impl(dest, len, value);
    }
}

#[no_mangle]
pub extern "C" fn afs_fill_f32(dest: *mut f32, n: i64, value: f32) {
    if dest.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        fill_f32_impl(dest, len, value);
    }
}

#[no_mangle]
pub extern "C" fn afs_fill_f64(dest: *mut f64, n: i64, value: f64) {
    if dest.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        fill_f64_impl(dest, len, value);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_add_i32(dest: *mut i32, lhs: *const i32, rhs: *const i32, n: i64) {
    if dest.is_null() || lhs.is_null() || rhs.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        add_i32_impl(dest, lhs, rhs, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_add_f32(dest: *mut f32, lhs: *const f32, rhs: *const f32, n: i64) {
    if dest.is_null() || lhs.is_null() || rhs.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        add_f32_impl(dest, lhs, rhs, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_add_f64(dest: *mut f64, lhs: *const f64, rhs: *const f64, n: i64) {
    if dest.is_null() || lhs.is_null() || rhs.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        add_f64_impl(dest, lhs, rhs, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_sub_i32(dest: *mut i32, lhs: *const i32, rhs: *const i32, n: i64) {
    if dest.is_null() || lhs.is_null() || rhs.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        sub_i32_impl(dest, lhs, rhs, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_sub_f32(dest: *mut f32, lhs: *const f32, rhs: *const f32, n: i64) {
    if dest.is_null() || lhs.is_null() || rhs.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        sub_f32_impl(dest, lhs, rhs, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_sub_f64(dest: *mut f64, lhs: *const f64, rhs: *const f64, n: i64) {
    if dest.is_null() || lhs.is_null() || rhs.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        sub_f64_impl(dest, lhs, rhs, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_mul_i32(dest: *mut i32, lhs: *const i32, rhs: *const i32, n: i64) {
    if dest.is_null() || lhs.is_null() || rhs.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        mul_i32_impl(dest, lhs, rhs, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_mul_f32(dest: *mut f32, lhs: *const f32, rhs: *const f32, n: i64) {
    if dest.is_null() || lhs.is_null() || rhs.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        mul_f32_impl(dest, lhs, rhs, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_mul_f64(dest: *mut f64, lhs: *const f64, rhs: *const f64, n: i64) {
    if dest.is_null() || lhs.is_null() || rhs.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        mul_f64_impl(dest, lhs, rhs, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_add_scalar_i32(dest: *mut i32, src: *const i32, scalar: i32, n: i64) {
    if dest.is_null() || src.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        add_scalar_i32_impl(dest, src, scalar, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_add_scalar_f32(dest: *mut f32, src: *const f32, scalar: f32, n: i64) {
    if dest.is_null() || src.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        add_scalar_f32_impl(dest, src, scalar, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_add_scalar_f64(dest: *mut f64, src: *const f64, scalar: f64, n: i64) {
    if dest.is_null() || src.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        add_scalar_f64_impl(dest, src, scalar, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_sub_scalar_i32(dest: *mut i32, src: *const i32, scalar: i32, n: i64) {
    if dest.is_null() || src.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        sub_scalar_i32_impl(dest, src, scalar, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_sub_scalar_f32(dest: *mut f32, src: *const f32, scalar: f32, n: i64) {
    if dest.is_null() || src.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        sub_scalar_f32_impl(dest, src, scalar, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_sub_scalar_f64(dest: *mut f64, src: *const f64, scalar: f64, n: i64) {
    if dest.is_null() || src.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        sub_scalar_f64_impl(dest, src, scalar, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_scalar_sub_array_i32(dest: *mut i32, scalar: i32, src: *const i32, n: i64) {
    if dest.is_null() || src.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        scalar_sub_i32_impl(dest, scalar, src, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_scalar_sub_array_f32(dest: *mut f32, scalar: f32, src: *const f32, n: i64) {
    if dest.is_null() || src.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        scalar_sub_f32_impl(dest, scalar, src, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_scalar_sub_array_f64(dest: *mut f64, scalar: f64, src: *const f64, n: i64) {
    if dest.is_null() || src.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        scalar_sub_f64_impl(dest, scalar, src, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_mul_scalar_i32(dest: *mut i32, src: *const i32, scalar: i32, n: i64) {
    if dest.is_null() || src.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        mul_scalar_i32_impl(dest, src, scalar, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_mul_scalar_f32(dest: *mut f32, src: *const f32, scalar: f32, n: i64) {
    if dest.is_null() || src.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        mul_scalar_f32_impl(dest, src, scalar, len);
    }
}

#[no_mangle]
pub extern "C" fn afs_array_mul_scalar_f64(dest: *mut f64, src: *const f64, scalar: f64, n: i64) {
    if dest.is_null() || src.is_null() {
        return;
    }
    let len = bulk_len(n);
    if len == 0 {
        return;
    }
    unsafe {
        mul_scalar_f64_impl(dest, src, scalar, len);
    }
}

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
        if !stat.is_null() {
            unsafe {
                *stat = 1;
            }
        }
        return;
    }

    let desc = unsafe { &mut *desc };

    // Check if already allocated.
    if desc.is_allocated() {
        if !stat.is_null() {
            unsafe {
                *stat = 2;
            } // already allocated
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
        if !stat.is_null() {
            unsafe {
                *stat = 0;
            }
        }
        return;
    }

    // Allocate.
    let ptr = unsafe { libc_malloc(bytes as usize) };
    if ptr.is_null() {
        if !stat.is_null() {
            unsafe {
                *stat = 3;
            } // allocation failed
            return;
        }
        eprintln!("ALLOCATE: out of memory ({} bytes)", bytes);
        std::process::exit(1);
    }

    // Zero-initialize (Fortran doesn't require this, but it's safer).
    unsafe {
        ptr::write_bytes(ptr, 0, bytes as usize);
    }

    desc.base_addr = ptr;
    desc.flags = DESC_ALLOCATED | DESC_CONTIGUOUS;

    if !stat.is_null() {
        unsafe {
            *stat = 0;
        }
    }
}

/// Simplified allocate for a 1D array with given element count.
/// Used by generated code for simple `allocate(a(n))` patterns.
#[no_mangle]
pub extern "C" fn afs_allocate_1d(desc: *mut ArrayDescriptor, elem_size: i64, n: i64) {
    let dim = DimDescriptor {
        lower_bound: 1,
        upper_bound: n,
        stride: 1,
    };
    afs_allocate_array(
        desc,
        elem_size,
        1,
        &dim as *const DimDescriptor,
        ptr::null_mut(),
    );
}

/// Allocate `dest` with the same shape and element size as `source`.
///
/// The resulting destination is always contiguous, even when `source`
/// is a section descriptor with non-unit strides.
#[no_mangle]
pub extern "C" fn afs_allocate_like(
    dest: *mut ArrayDescriptor,
    source: *const ArrayDescriptor,
    stat: *mut i32,
) {
    if dest.is_null() || source.is_null() {
        if !stat.is_null() {
            unsafe {
                *stat = 1;
            }
        }
        return;
    }

    let source = unsafe { &*source };
    let mut dims = [DimDescriptor::default(); MAX_RANK];
    for (i, dim) in dims.iter_mut().enumerate().take(source.rank as usize) {
        *dim = DimDescriptor {
            lower_bound: source.dims[i].lower_bound,
            upper_bound: source.dims[i].upper_bound,
            stride: 1,
        };
    }

    let dims_ptr = if source.rank > 0 {
        dims.as_ptr()
    } else {
        ptr::null()
    };
    afs_allocate_array(dest, source.elem_size, source.rank, dims_ptr, stat);
}

/// Copy array payload from `source` into an already-allocated `dest` without
/// reshaping or reallocating `dest`.
///
/// Used by `ALLOCATE(..., SOURCE=...)` after the destination shape has already
/// been fixed by explicit bounds. On mismatch, the fresh destination allocation
/// is rolled back so the overall statement still fails loudly instead of
/// silently changing shape.
#[no_mangle]
pub extern "C" fn afs_copy_array_data(
    dest: *mut ArrayDescriptor,
    source: *const ArrayDescriptor,
    stat: *mut i32,
) {
    if dest.is_null() || source.is_null() {
        if !stat.is_null() {
            unsafe {
                *stat = 1;
            }
            return;
        }
        eprintln!("ALLOCATE SOURCE=: null descriptor");
        std::process::exit(1);
    }

    let dest = unsafe { &mut *dest };
    let source = unsafe { &*source };

    let ok = dest.is_allocated()
        && source.is_allocated()
        && dest.elem_size == source.elem_size
        && dest.rank == source.rank
        && (0..dest.rank as usize).all(|i| dest.dims[i].extent() == source.dims[i].extent());

    if !ok {
        if dest.is_allocated() && !dest.base_addr.is_null() {
            unsafe {
                libc_free(dest.base_addr);
            }
        }
        dest.base_addr = ptr::null_mut();
        dest.flags &= !DESC_ALLOCATED;
        if !stat.is_null() {
            unsafe {
                *stat = 4;
            }
            return;
        }
        eprintln!("ALLOCATE SOURCE=: destination shape does not conform to source");
        std::process::exit(1);
    }

    let bytes = source.total_bytes();
    if bytes > 0 && !source.base_addr.is_null() && !dest.base_addr.is_null() {
        unsafe {
            ptr::copy(source.base_addr, dest.base_addr, bytes as usize);
        }
    }

    if !stat.is_null() {
        unsafe {
            *stat = 0;
        }
    }
}

// ---- DEALLOCATE ----

/// Deallocate an array, freeing its memory and clearing the descriptor.
///
/// Safe to call on an already-deallocated descriptor (no-op with stat=0).
#[no_mangle]
pub extern "C" fn afs_deallocate_array(desc: *mut ArrayDescriptor, stat: *mut i32) {
    if desc.is_null() {
        if !stat.is_null() {
            unsafe {
                *stat = 1;
            }
        }
        return;
    }

    let desc = unsafe { &mut *desc };

    if !desc.is_allocated() {
        // Not allocated — not an error with STAT, abort without STAT.
        if !stat.is_null() {
            unsafe {
                *stat = 0;
            }
            return;
        }
        // Without STAT, deallocating an unallocated array is an error.
        eprintln!("DEALLOCATE: array is not allocated");
        std::process::exit(1);
    }

    // Free the data.
    if !desc.base_addr.is_null() {
        unsafe {
            libc_free(desc.base_addr);
        }
    }

    // Clear the descriptor.
    desc.base_addr = ptr::null_mut();
    desc.flags &= !DESC_ALLOCATED;
    // Leave rank, elem_size, dims intact (they describe the shape for future allocate).

    if !stat.is_null() {
        unsafe {
            *stat = 0;
        }
    }
}

// ---- ALLOCATABLE ASSIGNMENT ----

fn descriptor_looks_sane(desc: &ArrayDescriptor) -> bool {
    let known_flags = DESC_ALLOCATED | DESC_CONTIGUOUS | DESC_POINTER;
    if desc.flags & !known_flags != 0 {
        return false;
    }
    if desc.rank < 0 || desc.rank as usize > MAX_RANK {
        return false;
    }
    if desc.elem_size < 0 {
        return false;
    }
    if desc.is_allocated() && desc.base_addr.is_null() {
        return false;
    }
    if !desc.is_allocated() && !desc.base_addr.is_null() {
        return false;
    }
    true
}

/// Assign one array to another with automatic reallocation (F2003).
///
/// If dest's shape doesn't match source's shape, deallocate dest and
/// reallocate with source's shape. Then copy data.
#[no_mangle]
pub extern "C" fn afs_assign_allocatable(
    dest: *mut ArrayDescriptor,
    source: *const ArrayDescriptor,
) {
    if dest.is_null() || source.is_null() {
        return;
    }

    let dest = unsafe { &mut *dest };
    let source = unsafe { &*source };

    if !descriptor_looks_sane(dest) {
        *dest = ArrayDescriptor::zeroed();
    }

    if !source.is_allocated() && source.base_addr.is_null() {
        if dest.is_allocated() && !dest.base_addr.is_null() {
            unsafe {
                libc_free(dest.base_addr);
            }
        }
        *dest = ArrayDescriptor::zeroed();
        return;
    }

    // Check if shapes match.
    let shapes_match = dest.rank == source.rank && {
        (0..dest.rank as usize).all(|i| dest.dims[i].extent() == source.dims[i].extent())
    };

    if !shapes_match || !dest.is_allocated() {
        // Deallocate dest if allocated.
        if dest.is_allocated() && !dest.base_addr.is_null() {
            unsafe {
                libc_free(dest.base_addr);
            }
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
pub extern "C" fn afs_move_alloc(from: *mut ArrayDescriptor, to: *mut ArrayDescriptor) {
    if from.is_null() || to.is_null() {
        return;
    }

    let from_desc = unsafe { &mut *from };
    let to_desc = unsafe { &mut *to };

    // Deallocate `to` if allocated.
    if to_desc.is_allocated() && !to_desc.base_addr.is_null() {
        unsafe {
            libc_free(to_desc.base_addr);
        }
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
    if desc.is_null() {
        return 0;
    }
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
    if source.is_null() || result.is_null() || specs.is_null() {
        return;
    }

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
            || (spec.stride < 0 && spec.start < spec.end)
        {
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
            DimDescriptor {
                lower_bound: 1,
                upper_bound: 3,
                stride: 1,
            },
            DimDescriptor {
                lower_bound: 1,
                upper_bound: 4,
                stride: 1,
            },
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
        let dim = DimDescriptor {
            lower_bound: 1,
            upper_bound: 10,
            stride: 1,
        };
        afs_allocate_array(&mut desc, 4, 1, &dim, &mut stat);
        assert_eq!(stat, 2); // already allocated
        afs_deallocate_array(&mut desc, ptr::null_mut());
    }

    #[test]
    fn allocate_like_preserves_shape_and_forces_contiguous_stride() {
        let mut source = ArrayDescriptor::zeroed();
        source.elem_size = 8;
        source.rank = 2;
        source.flags = DESC_ALLOCATED;
        source.dims[0] = DimDescriptor {
            lower_bound: -2,
            upper_bound: 1,
            stride: 3,
        };
        source.dims[1] = DimDescriptor {
            lower_bound: 4,
            upper_bound: 6,
            stride: 5,
        };

        let mut dest = ArrayDescriptor::zeroed();
        let mut stat = -1;
        afs_allocate_like(&mut dest, &source, &mut stat);
        assert_eq!(stat, 0);
        assert!(dest.is_allocated());
        assert_eq!(dest.elem_size, 8);
        assert_eq!(dest.rank, 2);
        assert_eq!(dest.dims[0].lower_bound, -2);
        assert_eq!(dest.dims[0].upper_bound, 1);
        assert_eq!(dest.dims[0].stride, 1);
        assert_eq!(dest.dims[1].lower_bound, 4);
        assert_eq!(dest.dims[1].upper_bound, 6);
        assert_eq!(dest.dims[1].stride, 1);

        afs_deallocate_array(&mut dest, ptr::null_mut());
    }

    #[test]
    fn copy_array_data_preserves_explicit_destination_shape() {
        let mut source = ArrayDescriptor::zeroed();
        let mut dest = ArrayDescriptor::zeroed();
        let mut stat = -1;

        afs_allocate_1d(&mut source, 4, 2);
        afs_allocate_1d(&mut dest, 4, 2);
        unsafe {
            let src = source.base_addr as *mut i32;
            *src.add(0) = 4;
            *src.add(1) = 5;
        }

        afs_copy_array_data(&mut dest, &source, &mut stat);
        assert_eq!(stat, 0);
        assert_eq!(dest.total_elements(), 2);
        unsafe {
            let data = dest.base_addr as *const i32;
            assert_eq!(*data.add(0), 4);
            assert_eq!(*data.add(1), 5);
        }

        afs_deallocate_array(&mut source, ptr::null_mut());
        afs_deallocate_array(&mut dest, ptr::null_mut());
    }

    #[test]
    fn copy_array_data_rolls_back_on_shape_mismatch() {
        let mut source = ArrayDescriptor::zeroed();
        let mut dest = ArrayDescriptor::zeroed();
        let mut stat = -1;

        afs_allocate_1d(&mut source, 4, 3);
        afs_allocate_1d(&mut dest, 4, 2);
        afs_copy_array_data(&mut dest, &source, &mut stat);
        assert_eq!(stat, 4);
        assert!(!dest.is_allocated());
        assert!(dest.base_addr.is_null());

        afs_deallocate_array(&mut source, ptr::null_mut());
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
    fn assign_allocatable_from_unallocated_source_clears_dest() {
        let source = ArrayDescriptor::zeroed();
        let mut dest = ArrayDescriptor::zeroed();

        afs_allocate_1d(&mut dest, 4, 3);
        assert!(dest.is_allocated());

        afs_assign_allocatable(&mut dest, &source);
        assert!(!dest.is_allocated());
        assert!(dest.base_addr.is_null());
        assert_eq!(dest.rank, 0);
    }

    #[test]
    fn assign_allocatable_ignores_invalid_garbage_dest_for_unallocated_source() {
        let source = ArrayDescriptor::zeroed();
        let mut dest = ArrayDescriptor::zeroed();

        dest.flags = DESC_ALLOCATED;
        dest.rank = 1;
        dest.elem_size = 4;
        dest.dims[0] = DimDescriptor {
            lower_bound: 1,
            upper_bound: 0,
            stride: 1,
        };

        afs_assign_allocatable(&mut dest, &source);
        assert!(!dest.is_allocated());
        assert!(dest.base_addr.is_null());
        assert_eq!(dest.rank, 0);
    }

    #[test]
    fn assign_allocatable_copies_from_nonowning_section_descriptor() {
        let mut backing = ArrayDescriptor::zeroed();
        let mut section = ArrayDescriptor::zeroed();
        let mut dest = ArrayDescriptor::zeroed();

        afs_allocate_1d(&mut backing, 4, 4);
        unsafe {
            let data = backing.base_addr as *mut i32;
            *data.add(0) = 10;
            *data.add(1) = 20;
            *data.add(2) = 30;
            *data.add(3) = 40;
        }

        let spec = SectionSpec {
            start: 2,
            end: 3,
            stride: 1,
        };
        afs_create_section(&backing, &mut section, &spec, 1);
        assert!(!section.is_allocated());
        assert!(!section.base_addr.is_null());

        afs_assign_allocatable(&mut dest, &section);
        assert!(dest.is_allocated());
        assert_eq!(dest.total_elements(), 2);
        unsafe {
            let data = dest.base_addr as *const i32;
            assert_eq!(*data.add(0), 20);
            assert_eq!(*data.add(1), 30);
        }

        afs_deallocate_array(&mut backing, ptr::null_mut());
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

    #[test]
    fn fill_i32_bulk_kernel() {
        let mut data = [0_i32; 8];
        afs_fill_i32(data.as_mut_ptr(), data.len() as i64, 7);
        assert_eq!(data, [7, 7, 7, 7, 7, 7, 7, 7]);
    }

    #[test]
    fn array_add_i32_bulk_kernel() {
        let lhs = [1_i32, 2, 3, 4, 5, 6, 7, 8];
        let rhs = [10_i32, 20, 30, 40, 50, 60, 70, 80];
        let mut out = [0_i32; 8];
        afs_array_add_i32(
            out.as_mut_ptr(),
            lhs.as_ptr(),
            rhs.as_ptr(),
            out.len() as i64,
        );
        assert_eq!(out, [11, 22, 33, 44, 55, 66, 77, 88]);
    }

    #[test]
    fn array_add_f64_bulk_kernel() {
        let lhs = [1.5_f64, 2.5, 3.5, 4.5];
        let rhs = [10.0_f64, 20.0, 30.0, 40.0];
        let mut out = [0.0_f64; 4];
        afs_array_add_f64(
            out.as_mut_ptr(),
            lhs.as_ptr(),
            rhs.as_ptr(),
            out.len() as i64,
        );
        assert_eq!(out, [11.5, 22.5, 33.5, 44.5]);
    }

    #[test]
    fn array_sub_i32_bulk_kernel() {
        let lhs = [11_i32, 22, 33, 44, 55, 66, 77, 88];
        let rhs = [1_i32, 2, 3, 4, 5, 6, 7, 8];
        let mut out = [0_i32; 8];
        afs_array_sub_i32(
            out.as_mut_ptr(),
            lhs.as_ptr(),
            rhs.as_ptr(),
            out.len() as i64,
        );
        assert_eq!(out, [10, 20, 30, 40, 50, 60, 70, 80]);
    }

    #[test]
    fn array_mul_f32_bulk_kernel() {
        let lhs = [1.0_f32, 2.0, 3.0, 4.0];
        let rhs = [2.0_f32, 3.0, 4.0, 5.0];
        let mut out = [0.0_f32; 4];
        afs_array_mul_f32(
            out.as_mut_ptr(),
            lhs.as_ptr(),
            rhs.as_ptr(),
            out.len() as i64,
        );
        assert_eq!(out, [2.0, 6.0, 12.0, 20.0]);
    }

    #[test]
    fn array_add_scalar_i32_bulk_kernel() {
        let src = [1_i32, 2, 3, 4, 5, 6, 7, 8];
        let mut out = [0_i32; 8];
        afs_array_add_scalar_i32(out.as_mut_ptr(), src.as_ptr(), 5, out.len() as i64);
        assert_eq!(out, [6, 7, 8, 9, 10, 11, 12, 13]);
    }

    #[test]
    fn scalar_sub_array_i32_bulk_kernel() {
        let src = [1_i32, 2, 3, 4, 5, 6, 7, 8];
        let mut out = [0_i32; 8];
        afs_scalar_sub_array_i32(out.as_mut_ptr(), 20, src.as_ptr(), out.len() as i64);
        assert_eq!(out, [19, 18, 17, 16, 15, 14, 13, 12]);
    }

    #[test]
    fn array_mul_scalar_f64_bulk_kernel() {
        let src = [1.5_f64, 2.5, 3.5, 4.5];
        let mut out = [0.0_f64; 4];
        afs_array_mul_scalar_f64(out.as_mut_ptr(), src.as_ptr(), 2.0, out.len() as i64);
        assert_eq!(out, [3.0, 5.0, 7.0, 9.0]);
    }
}

// ---- Array query intrinsics ----

/// SIZE(array) — total number of elements.
#[no_mangle]
pub extern "C" fn afs_array_size(desc: *const ArrayDescriptor) -> i64 {
    if desc.is_null() {
        return 0;
    }
    unsafe { (*desc).total_elements() }
}

/// SIZE(array, dim) — number of elements along dimension `dim` (1-based).
#[no_mangle]
pub extern "C" fn afs_array_size_dim(desc: *const ArrayDescriptor, dim: i32) -> i64 {
    if desc.is_null() || dim < 1 {
        return 0;
    }
    let d = unsafe { &*desc };
    let idx = (dim - 1) as usize;
    if idx < d.rank as usize {
        d.dims[idx].extent()
    } else {
        0
    }
}

/// LBOUND(array, dim) — lower bound along dimension `dim` (1-based).
#[no_mangle]
pub extern "C" fn afs_array_lbound(desc: *const ArrayDescriptor, dim: i32) -> i64 {
    if desc.is_null() || dim < 1 {
        return 1;
    }
    let d = unsafe { &*desc };
    let idx = (dim - 1) as usize;
    if idx < d.rank as usize {
        d.dims[idx].lower_bound
    } else {
        1
    }
}

/// UBOUND(array, dim) — upper bound along dimension `dim` (1-based).
#[no_mangle]
pub extern "C" fn afs_array_ubound(desc: *const ArrayDescriptor, dim: i32) -> i64 {
    if desc.is_null() || dim < 1 {
        return 0;
    }
    let d = unsafe { &*desc };
    let idx = (dim - 1) as usize;
    if idx < d.rank as usize {
        d.dims[idx].upper_bound
    } else {
        0
    }
}

/// ALLOCATED(array) — check if array is allocated (returns 1 or 0).
#[no_mangle]
pub extern "C" fn afs_array_allocated(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() {
        return 0;
    }
    unsafe { (*desc).is_allocated() as i32 }
}

/// SUM(array) — sum all elements (real(8) version).
/// Respects strides for non-contiguous sections.
#[no_mangle]
pub extern "C" fn afs_array_sum_real8(desc: *const ArrayDescriptor) -> f64 {
    if desc.is_null() {
        return 0.0;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 0.0;
    }
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
    if desc.is_null() {
        return 0;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 0;
    }
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
    if desc.is_null() {
        return 1.0;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 1.0;
    }
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
    if desc.is_null() {
        return 1;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 1;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return 1;
    }
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
    if desc.is_null() {
        return f64::NEG_INFINITY;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return f64::NEG_INFINITY;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return f64::NEG_INFINITY;
    }
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const f64;
    let mut max = unsafe { *ptr };
    for i in 1..n {
        let v = unsafe { *ptr.add(i * stride) };
        if v > max {
            max = v;
        }
    }
    max
}

/// MINVAL(array) — minimum element (real(8) version). Respects strides.
#[no_mangle]
pub extern "C" fn afs_array_minval_real8(desc: *const ArrayDescriptor) -> f64 {
    if desc.is_null() {
        return f64::INFINITY;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return f64::INFINITY;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return f64::INFINITY;
    }
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const f64;
    let mut min = unsafe { *ptr };
    for i in 1..n {
        let v = unsafe { *ptr.add(i * stride) };
        if v < min {
            min = v;
        }
    }
    min
}

/// MAXVAL(array) — maximum element (integer(4) version). Respects strides.
#[no_mangle]
pub extern "C" fn afs_array_maxval_int(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() {
        return i32::MIN;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return i32::MIN;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return i32::MIN;
    }
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const i32;
    let mut max = unsafe { *ptr };
    for i in 1..n {
        let v = unsafe { *ptr.add(i * stride) };
        if v > max {
            max = v;
        }
    }
    max
}

/// MINVAL(array) — minimum element (integer(4) version). Respects strides.
#[no_mangle]
pub extern "C" fn afs_array_minval_int(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() {
        return i32::MAX;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return i32::MAX;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return i32::MAX;
    }
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const i32;
    let mut min = unsafe { *ptr };
    for i in 1..n {
        let v = unsafe { *ptr.add(i * stride) };
        if v < min {
            min = v;
        }
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
    if source.is_null() || result.is_null() {
        return;
    }
    let src = unsafe { &*source };
    if src.rank < 2 || src.base_addr.is_null() {
        return;
    }

    let m = src.dims[0].extent() as usize;
    let n = src.dims[1].extent() as usize;
    let sp = src.base_addr as *const f64;

    // Allocate result as (n x m).
    afs_allocate_1d(result, 8, (n * m) as i64);
    let res = unsafe { &mut *result };
    res.rank = 2;
    res.dims[0] = DimDescriptor {
        lower_bound: 1,
        upper_bound: n as i64,
        stride: 1,
    };
    res.dims[1] = DimDescriptor {
        lower_bound: 1,
        upper_bound: m as i64,
        stride: 1,
    };
    let rp = res.base_addr as *mut f64;

    for i in 0..m {
        for j in 0..n {
            unsafe {
                *rp.add(j * m + i) = *sp.add(i * n + j);
            }
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
    if a.is_null() || b.is_null() || result.is_null() {
        return;
    }
    let da = unsafe { &*a };
    let db = unsafe { &*b };
    if da.base_addr.is_null() || db.base_addr.is_null() {
        return;
    }

    let m = da.dims[0].extent() as usize;
    let k = if da.rank >= 2 {
        da.dims[1].extent() as usize
    } else {
        1
    };
    let n = if db.rank >= 2 {
        db.dims[1].extent() as usize
    } else {
        db.dims[0].extent() as usize
    };

    // For vector * matrix or matrix * vector, adjust dimensions.
    let ap = da.base_addr as *const f64;
    let bp = db.base_addr as *const f64;

    // Allocate result.
    afs_allocate_1d(result, 8, (m * n) as i64);
    let res = unsafe { &mut *result };
    res.rank = 2;
    res.dims[0] = DimDescriptor {
        lower_bound: 1,
        upper_bound: m as i64,
        stride: 1,
    };
    res.dims[1] = DimDescriptor {
        lower_bound: 1,
        upper_bound: n as i64,
        stride: 1,
    };
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
            unsafe {
                *rp.add(i * n + j) = sum;
            }
        }
    }
}

/// MATMUL(a, b, result) — matrix multiplication (integer(4) version).
#[no_mangle]
pub extern "C" fn afs_matmul_int(
    a: *const ArrayDescriptor,
    b: *const ArrayDescriptor,
    result: *mut ArrayDescriptor,
) {
    if a.is_null() || b.is_null() || result.is_null() {
        return;
    }
    let da = unsafe { &*a };
    let db = unsafe { &*b };
    if da.base_addr.is_null() || db.base_addr.is_null() {
        return;
    }

    let m = da.dims[0].extent() as usize;
    let k = if da.rank >= 2 {
        da.dims[1].extent() as usize
    } else {
        1
    };
    let n = if db.rank >= 2 {
        db.dims[1].extent() as usize
    } else {
        db.dims[0].extent() as usize
    };

    let ap = da.base_addr as *const i32;
    let bp = db.base_addr as *const i32;

    afs_allocate_1d(result, 4, (m * n) as i64);
    let res = unsafe { &mut *result };
    res.rank = 2;
    res.dims[0] = DimDescriptor {
        lower_bound: 1,
        upper_bound: m as i64,
        stride: 1,
    };
    res.dims[1] = DimDescriptor {
        lower_bound: 1,
        upper_bound: n as i64,
        stride: 1,
    };
    let rp = res.base_addr as *mut i32;

    for i in 0..m {
        for j in 0..n {
            let mut sum: i64 = 0;
            for l in 0..k {
                let a_val = unsafe { *ap.add(i * k + l) as i64 };
                let b_val = unsafe { *bp.add(l * n + j) as i64 };
                sum += a_val * b_val;
            }
            unsafe {
                *rp.add(i * n + j) = sum as i32;
            }
        }
    }
}

/// TRANSPOSE(source, result) — matrix transpose (integer(4) version).
#[no_mangle]
pub extern "C" fn afs_transpose_int(source: *const ArrayDescriptor, result: *mut ArrayDescriptor) {
    if source.is_null() || result.is_null() {
        return;
    }
    let src = unsafe { &*source };
    if src.rank < 2 || src.base_addr.is_null() {
        return;
    }

    let m = src.dims[0].extent() as usize;
    let n = src.dims[1].extent() as usize;
    let sp = src.base_addr as *const i32;

    afs_allocate_1d(result, 4, (n * m) as i64);
    let res = unsafe { &mut *result };
    res.rank = 2;
    res.dims[0] = DimDescriptor {
        lower_bound: 1,
        upper_bound: n as i64,
        stride: 1,
    };
    res.dims[1] = DimDescriptor {
        lower_bound: 1,
        upper_bound: m as i64,
        stride: 1,
    };
    let rp = res.base_addr as *mut i32;

    for i in 0..m {
        for j in 0..n {
            unsafe {
                *rp.add(j * m + i) = *sp.add(i * n + j);
            }
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
    if a.is_null() || b.is_null() {
        return 0.0;
    }
    let da = unsafe { &*a };
    let db = unsafe { &*b };
    if da.base_addr.is_null() || db.base_addr.is_null() {
        return 0.0;
    }
    let n = da.dims[0].extent().min(db.dims[0].extent()) as usize;
    let stride_a = da.dims[0].stride.max(1) as usize;
    let stride_b = db.dims[0].stride.max(1) as usize;
    let pa = da.base_addr as *const f64;
    let pb = db.base_addr as *const f64;
    let mut dot = 0.0;
    for i in 0..n {
        dot += unsafe { *pa.add(i * stride_a) * *pb.add(i * stride_b) };
    }
    dot
}

/// DOT_PRODUCT(a, b) — vector dot product (real(4) version).
#[no_mangle]
pub extern "C" fn afs_dot_product_real4(
    a: *const ArrayDescriptor,
    b: *const ArrayDescriptor,
) -> f32 {
    if a.is_null() || b.is_null() {
        return 0.0;
    }
    let da = unsafe { &*a };
    let db = unsafe { &*b };
    if da.base_addr.is_null() || db.base_addr.is_null() {
        return 0.0;
    }
    let n = da.dims[0].extent().min(db.dims[0].extent()) as usize;
    let stride_a = da.dims[0].stride.max(1) as usize;
    let stride_b = db.dims[0].stride.max(1) as usize;
    let pa = da.base_addr as *const f32;
    let pb = db.base_addr as *const f32;
    let mut dot = 0.0;
    for i in 0..n {
        dot += unsafe { *pa.add(i * stride_a) * *pb.add(i * stride_b) };
    }
    dot
}

/// DOT_PRODUCT(a, b) — vector dot product (integer(4) version).
#[no_mangle]
pub extern "C" fn afs_dot_product_int(a: *const ArrayDescriptor, b: *const ArrayDescriptor) -> i64 {
    if a.is_null() || b.is_null() {
        return 0;
    }
    let da = unsafe { &*a };
    let db = unsafe { &*b };
    if da.base_addr.is_null() || db.base_addr.is_null() {
        return 0;
    }
    let n = da.dims[0].extent().min(db.dims[0].extent()) as usize;
    let stride_a = da.dims[0].stride.max(1) as usize;
    let stride_b = db.dims[0].stride.max(1) as usize;
    let pa = da.base_addr as *const i32;
    let pb = db.base_addr as *const i32;
    let mut dot: i64 = 0;
    for i in 0..n {
        dot += unsafe { (*pa.add(i * stride_a) as i64) * (*pb.add(i * stride_b) as i64) };
    }
    dot
}
