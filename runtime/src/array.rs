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

fn report_allocation_failure(stat: *mut i32, code: i32, message: &str) {
    if !stat.is_null() {
        unsafe {
            *stat = code;
        }
        return;
    }
    eprintln!("ALLOCATE: {}", message);
    std::process::exit(1);
}

fn checked_dim_extent(dim: DimDescriptor) -> Option<i64> {
    if dim.upper_bound < dim.lower_bound {
        Some(0)
    } else {
        dim.upper_bound.checked_sub(dim.lower_bound)?.checked_add(1)
    }
}

struct CheckedAllocationLayout {
    dims: [DimDescriptor; MAX_RANK],
    bytes: usize,
}

fn checked_allocation_layout(
    elem_size: i64,
    rank: i32,
    dims_ptr: *const DimDescriptor,
) -> Result<CheckedAllocationLayout, &'static str> {
    if rank < 0 || rank as usize > MAX_RANK {
        return Err("rank is outside the supported range");
    }
    if elem_size < 0 {
        return Err("element size is negative");
    }

    let rank = rank as usize;
    if rank > 0 && dims_ptr.is_null() {
        return Err("dimension descriptor is missing");
    }
    let input_dims = if rank == 0 {
        &[][..]
    } else {
        unsafe { std::slice::from_raw_parts(dims_ptr, rank) }
    };

    let mut dims = [DimDescriptor::default(); MAX_RANK];
    let mut total = 1i64;
    let mut running_stride = 1i64;
    for (i, input) in input_dims.iter().copied().enumerate() {
        let extent = checked_dim_extent(input).ok_or("dimension extent overflows")?;
        dims[i] = DimDescriptor {
            lower_bound: input.lower_bound,
            upper_bound: input.upper_bound,
            stride: running_stride,
        };
        total = total.checked_mul(extent).ok_or("element count overflows")?;
        running_stride = running_stride
            .checked_mul(extent.max(1))
            .ok_or("contiguous stride overflows")?;
    }

    let bytes = total
        .checked_mul(elem_size)
        .and_then(|value| usize::try_from(value).ok())
        .ok_or("allocation byte count overflows")?;
    Ok(CheckedAllocationLayout { dims, bytes })
}

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
        report_allocation_failure(stat, 1, "null array descriptor");
        return;
    }

    // Check if already allocated.
    if unsafe { &*desc }.is_allocated() {
        report_allocation_failure(stat, 2, "array is already allocated");
        return;
    }

    let layout = match checked_allocation_layout(elem_size, rank, dims_ptr) {
        Ok(layout) => layout,
        Err(message) => {
            report_allocation_failure(stat, 4, message);
            return;
        }
    };

    let data = if layout.bytes == 0 {
        ptr::null_mut()
    } else {
        let data = unsafe { libc_malloc(layout.bytes) };
        if data.is_null() {
            report_allocation_failure(stat, 3, "out of memory");
            return;
        }
        unsafe {
            ptr::write_bytes(data, 0, layout.bytes);
        }
        data
    };

    // Publish descriptor state only after every validation and allocation step
    // has succeeded, so STAT recovery never exposes a partial allocation.
    let desc = unsafe { &mut *desc };
    desc.rank = rank;
    desc.elem_size = elem_size;
    for (i, dim) in layout.dims.iter().copied().enumerate().take(rank as usize) {
        desc.dims[i] = dim;
    }
    desc.clear_dynamic_type_metadata();
    desc.base_addr = data;
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

/// Allocate `dest` with the same SHAPE (rank + extents) as `source`,
/// but a caller-provided `elem_size` and a 1-based bound view per
/// F2018 §10.1.5 / §6.5.3.5(2): elemental and relational array
/// expressions yield a result whose lower bound is 1 in every
/// dimension regardless of the operand's bounds.  Used by the
/// rank-N relational path so callees receiving the mask through
/// e.g. `mask(:,:)` see a coherent rank-N descriptor instead of
/// the rank-1 placeholder the old path emitted.
#[no_mangle]
pub extern "C" fn afs_allocate_like_with_elem_size(
    dest: *mut ArrayDescriptor,
    source: *const ArrayDescriptor,
    elem_size: i64,
    stat: *mut i32,
) {
    if dest.is_null() || source.is_null() {
        report_allocation_failure(stat, 1, "null array descriptor");
        return;
    }

    let source = unsafe { &*source };
    if source.rank < 0 || source.rank as usize > MAX_RANK {
        report_allocation_failure(stat, 4, "source rank is outside the supported range");
        return;
    }
    let mut dims = [DimDescriptor::default(); MAX_RANK];
    for (i, dim) in dims.iter_mut().enumerate().take(source.rank as usize) {
        let Some(extent) = checked_dim_extent(source.dims[i]) else {
            report_allocation_failure(stat, 4, "source dimension extent overflows");
            return;
        };
        *dim = DimDescriptor {
            lower_bound: 1,
            upper_bound: extent,
            stride: 1,
        };
    }

    let dims_ptr = if source.rank > 0 {
        dims.as_ptr()
    } else {
        ptr::null()
    };
    afs_allocate_array(dest, elem_size, source.rank, dims_ptr, stat);
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
        report_allocation_failure(stat, 1, "null array descriptor");
        return;
    }

    let source = unsafe { &*source };
    let elem_size = source.elem_size;
    let rank = source.rank;
    let scalar_type_tag = source.scalar_type_tag();
    let dynamic_vtable = source.dynamic_vtable_ptr();
    let dims_ptr = source.dims.as_ptr();
    afs_allocate_array(dest, elem_size, rank, dims_ptr, stat);
    if !stat.is_null() && unsafe { *stat } != 0 {
        return;
    }
    let dest = unsafe { &mut *dest };
    dest.set_scalar_type_tag(scalar_type_tag);
    dest.set_dynamic_vtable_ptr(dynamic_vtable);
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
    afs_prepare_array_copy(dest, source, stat);
    if !stat.is_null() {
        let status = unsafe { *stat };
        if status != 0 {
            return;
        }
    }

    let dest = unsafe { &mut *dest };
    let source = unsafe { &*source };
    let bytes = source.total_bytes();
    if bytes > 0 && !source.base_addr.is_null() && !dest.base_addr.is_null() {
        // F2018 §9.7.1.2: SOURCE-expr may be a non-contiguous section
        // (e.g. `allocate(col, source = idx(2, 1:n))` reads only every
        // other element of `idx`). A flat ptr::copy treats the source
        // base..base+total_bytes as contiguous, picking up adjacent
        // dim-0 entries instead of stepping by the per-dim memory
        // stride. Detect non-contiguous (any source stride != the
        // canonical column-major step) and walk element-by-element.
        let elem_size = source.elem_size;
        let mut canonical: i64 = 1;
        let mut contiguous = true;
        for i in 0..source.rank as usize {
            if source.dims[i].stride != canonical {
                contiguous = false;
                break;
            }
            canonical = canonical.saturating_mul(source.dims[i].extent().max(1));
        }
        if contiguous {
            unsafe {
                ptr::copy(source.base_addr, dest.base_addr, bytes as usize);
            }
        } else {
            // Walk every multi-index of the source in column-major
            // order and copy `elem_size` bytes per slot. Dest is
            // contiguous (just allocated) so its destination index
            // is the linear count.
            let rank = source.rank as usize;
            let extents: Vec<i64> = (0..rank).map(|i| source.dims[i].extent()).collect();
            let strides: Vec<i64> = (0..rank).map(|i| source.dims[i].stride).collect();
            let mut idx = vec![0i64; rank];
            let total = source.total_elements();
            for k in 0..total {
                let mut src_off: i64 = 0;
                for d in 0..rank {
                    src_off += idx[d] * strides[d];
                }
                src_off *= elem_size;
                let dst_off = k * elem_size;
                unsafe {
                    ptr::copy_nonoverlapping(
                        source.base_addr.offset(src_off as isize),
                        dest.base_addr.offset(dst_off as isize),
                        elem_size as usize,
                    );
                }
                // increment column-major: dim 0 fastest
                for d in 0..rank {
                    idx[d] += 1;
                    if idx[d] < extents[d] {
                        break;
                    }
                    idx[d] = 0;
                }
            }
        }
    }
    dest.set_scalar_type_tag(source.scalar_type_tag());
    dest.set_dynamic_vtable_ptr(source.dynamic_vtable_ptr());

    if !stat.is_null() {
        unsafe {
            *stat = 0;
        }
    }
}

/// Copy an array descriptor result into a fixed-shape caller buffer.
///
/// Fixed-shape assignment from an allocatable function result cannot hand the
/// callee the caller's raw stack buffer as the hidden result slot, so generated
/// code receives a descriptor temp and then copies payload bytes back into the
/// fixed destination. A valid zero-size result has `base_addr == NULL`; treat
/// it as a zero-fill instead of forwarding a null source pointer to `memcpy`.
#[no_mangle]
pub extern "C" fn afs_copy_array_result_to_fixed(
    dest: *mut u8,
    source: *const ArrayDescriptor,
    dest_bytes: i64,
) {
    if dest.is_null() || dest_bytes <= 0 {
        return;
    }

    let dest_len = dest_bytes as usize;
    unsafe {
        ptr::write_bytes(dest, 0, dest_len);
    }

    if source.is_null() {
        return;
    }

    let source = unsafe { &*source };
    let source_bytes = source.total_bytes();
    if source_bytes <= 0 || source.base_addr.is_null() {
        return;
    }

    let copy_bytes = dest_bytes.min(source_bytes) as usize;
    if copy_bytes == 0 {
        return;
    }

    unsafe {
        ptr::copy(source.base_addr, dest, copy_bytes);
    }
}

#[no_mangle]
pub extern "C" fn afs_copy_array_result_to_fixed_convert(
    dest: *mut u8,
    source: *const ArrayDescriptor,
    dest_bytes: i64,
    dest_kind_tag: i32,
    src_kind_tag: i32,
) {
    if dest.is_null() || dest_bytes <= 0 {
        return;
    }

    let dest_len = dest_bytes as usize;
    unsafe {
        ptr::write_bytes(dest, 0, dest_len);
    }

    let Some(dest_elem_size) = numeric_kind_elem_size(dest_kind_tag) else {
        return;
    };
    let Some(src_elem_size) = numeric_kind_elem_size(src_kind_tag) else {
        return;
    };
    let max_dest_elems = dest_bytes / dest_elem_size;
    if max_dest_elems <= 0 || source.is_null() {
        return;
    }

    let source = unsafe { &*source };
    if source.base_addr.is_null() || source.total_bytes() <= 0 {
        return;
    }

    let rank = source.rank.max(0) as usize;
    let source_elems = source.total_elements().max(0);
    let n = source_elems.min(max_dest_elems) as usize;
    if n == 0 {
        return;
    }

    let extents: Vec<i64> = (0..rank).map(|i| source.dims[i].extent()).collect();
    let raw_strides: Vec<i64> = (0..rank).map(|i| source.dims[i].stride).collect();
    let mut canonical_step: i64 = 1;
    let mut canonical: Vec<i64> = Vec::with_capacity(rank);
    let mut strided = false;
    for d in 0..rank {
        canonical.push(canonical_step);
        if raw_strides[d] < 0 || raw_strides[d] > canonical_step {
            strided = true;
        }
        canonical_step = canonical_step.saturating_mul(extents[d].max(1));
    }
    let strides: &[i64] = if strided { &raw_strides } else { &canonical };
    let mut idx = vec![0i64; rank];

    for k in 0..n {
        let mut src_off_elems: i64 = 0;
        for d in 0..rank {
            src_off_elems += idx[d] * strides[d];
        }
        let src_byte_off = src_off_elems * src_elem_size;
        let (src_re_f64, src_im_f64): (f64, f64) = unsafe {
            match src_kind_tag {
                0 => (
                    *(source.base_addr.offset(src_byte_off as isize) as *const i8) as f64,
                    0.0,
                ),
                1 => (
                    *(source.base_addr.offset(src_byte_off as isize) as *const i16) as f64,
                    0.0,
                ),
                2 => (
                    *(source.base_addr.offset(src_byte_off as isize) as *const i32) as f64,
                    0.0,
                ),
                3 => (
                    *(source.base_addr.offset(src_byte_off as isize) as *const i64) as f64,
                    0.0,
                ),
                4 => (
                    *(source.base_addr.offset(src_byte_off as isize) as *const f32) as f64,
                    0.0,
                ),
                5 => (
                    *(source.base_addr.offset(src_byte_off as isize) as *const f64),
                    0.0,
                ),
                6 => {
                    let p = source.base_addr.offset(src_byte_off as isize) as *const f32;
                    ((*p) as f64, (*p.add(1)) as f64)
                }
                7 => {
                    let p = source.base_addr.offset(src_byte_off as isize) as *const f64;
                    (*p, *p.add(1))
                }
                _ => return,
            }
        };
        unsafe {
            match dest_kind_tag {
                0 => *(dest.add(k) as *mut i8) = src_re_f64 as i8,
                1 => *(dest.add(2 * k) as *mut i16) = src_re_f64 as i16,
                2 => *(dest.add(4 * k) as *mut i32) = src_re_f64 as i32,
                3 => *(dest.add(8 * k) as *mut i64) = src_re_f64 as i64,
                4 => *(dest.add(4 * k) as *mut f32) = src_re_f64 as f32,
                5 => *(dest.add(8 * k) as *mut f64) = src_re_f64,
                6 => {
                    let p = dest.add(8 * k) as *mut f32;
                    *p = src_re_f64 as f32;
                    *p.add(1) = src_im_f64 as f32;
                }
                7 => {
                    let p = dest.add(16 * k) as *mut f64;
                    *p = src_re_f64;
                    *p.add(1) = src_im_f64;
                }
                _ => return,
            }
        }
        for d in 0..rank {
            idx[d] += 1;
            if idx[d] < extents[d] {
                break;
            }
            idx[d] = 0;
        }
    }
}

fn descriptor_payload_requires_zero_bytes(desc: &ArrayDescriptor) -> bool {
    let Ok(rank) = usize::try_from(desc.rank) else {
        return false;
    };
    if rank > MAX_RANK || desc.elem_size < 0 {
        return false;
    }

    let mut has_zero_extent = false;
    for dim in desc.dims.iter().copied().take(rank) {
        let Some(extent) = checked_dim_extent(dim) else {
            return false;
        };
        has_zero_extent |= extent == 0;
    }

    desc.elem_size == 0 || has_zero_extent
}

/// Validate `ALLOCATE(..., SOURCE=...)` array conformance after the destination
/// has already been allocated with its final shape.
///
/// On mismatch, the fresh destination allocation is rolled back so the overall
/// statement still fails loudly instead of silently changing shape.
#[no_mangle]
pub extern "C" fn afs_prepare_array_copy(
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

    // F2018 §9.7.1.2: SOURCE-expr need only have a defined value of the
    // right type/kind/shape — it doesn't have to be an ALLOCATABLE.
    // Common case: `allocate(amat(...), source=a)` where `a` is an
    // assumed-shape dummy `a(:,:)`.  Such dummies have flags=CONTIGUOUS
    // (no DESC_ALLOCATED) since they're bound to the caller's data, not
    // owned. A null base_addr is also valid when the descriptor's shape
    // or element size requires zero storage. Require DESC_ALLOCATED only
    // on the freshly-allocated destination.
    let source_has_defined_storage =
        !source.base_addr.is_null() || descriptor_payload_requires_zero_bytes(source);
    let ok = dest.is_allocated()
        && source_has_defined_storage
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

    if !stat.is_null() {
        unsafe {
            *stat = 0;
        }
    }
}

// ---- DEALLOCATE ----

/// Deallocate an array, freeing its memory and clearing the descriptor.
///
/// Reports an error when the descriptor is not allocated.
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
        if !stat.is_null() {
            unsafe {
                *stat = 2;
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
    desc.clear_dynamic_type_metadata();
    // Leave rank, elem_size, dims intact (they describe the shape for future allocate).

    if !stat.is_null() {
        unsafe {
            *stat = 0;
        }
    }
}

// ---- ALLOCATABLE ASSIGNMENT ----

fn descriptor_looks_sane(desc: &ArrayDescriptor) -> bool {
    let known_flags = DESC_ALLOCATED | DESC_CONTIGUOUS | DESC_POINTER | DESC_TYPE_TAG_MASK;
    if desc.flags & !known_flags != 0 {
        return false;
    }
    if desc.rank < 0 || desc.rank as usize > MAX_RANK {
        return false;
    }
    if desc.elem_size < 0 {
        return false;
    }
    if desc.is_allocated() && desc.base_addr.is_null() && !descriptor_is_zero_size_array(desc) {
        return false;
    }
    if !desc.is_allocated() && !desc.base_addr.is_null() {
        return false;
    }
    true
}

fn descriptor_is_zero_size_array(desc: &ArrayDescriptor) -> bool {
    desc.rank > 0 && desc.elem_size > 0 && desc.total_elements() == 0
}

fn descriptor_has_payload_or_zero_size_array(desc: &ArrayDescriptor) -> bool {
    !desc.base_addr.is_null() || descriptor_is_zero_size_array(desc)
}

/// Return nonzero when character-array assignment must replace `dest`'s
/// allocation to conform to `source` and the destination element length.
#[no_mangle]
pub extern "C" fn afs_char_array_assignment_requires_reallocation(
    dest: *const ArrayDescriptor,
    source: *const ArrayDescriptor,
    dest_elem_size: i64,
) -> i32 {
    if dest.is_null() || source.is_null() || dest_elem_size < 0 {
        return 1;
    }

    let dest = unsafe { &*dest };
    let source = unsafe { &*source };
    let source_rank_is_valid = source.rank >= 0 && source.rank as usize <= MAX_RANK;
    let dest_rank_is_valid = dest.rank >= 0 && dest.rank as usize <= MAX_RANK;
    let known_flags = DESC_ALLOCATED | DESC_CONTIGUOUS | DESC_POINTER | DESC_TYPE_TAG_MASK;
    let dest_has_zero_byte_payload = dest_rank_is_valid
        && dest.rank > 0
        && (dest.elem_size == 0 || (0..dest.rank as usize).any(|i| dest.dims[i].extent() == 0));
    let dest_has_valid_storage = !dest.base_addr.is_null() || dest_has_zero_byte_payload;
    let dest_is_valid = dest.flags & !known_flags == 0
        && dest_rank_is_valid
        && dest.elem_size >= 0
        && dest.is_allocated()
        && dest_has_valid_storage;
    let conforms = source_rank_is_valid
        && dest_is_valid
        && dest.elem_size == dest_elem_size
        && dest.rank == source.rank
        && (0..dest.rank as usize).all(|i| dest.dims[i].extent() == source.dims[i].extent());

    if conforms {
        0
    } else {
        1
    }
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

    if !source.is_allocated()
        && source.base_addr.is_null()
        && !descriptor_is_zero_size_array(source)
    {
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

    let source_snapshot = if source_base_points_into_dest_storage(dest, source) {
        let bytes = source.total_bytes();
        if bytes > 0 {
            let mut buf = vec![0u8; bytes as usize];
            unsafe {
                copy_same_type_payload_to_contiguous(source, buf.as_mut_ptr());
            }
            Some(buf)
        } else {
            None
        }
    } else {
        None
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

        // Allocate with source's shape, but compute canonical
        // column-major strides (1, ext_0, ext_0*ext_1, ...) — the
        // dest is freshly contiguous, so per-dim memory step must
        // match Fortran's column-major convention used by
        // afs_create_section / load_rank1_array_desc_elem. Setting
        // stride=1 across the board collapsed dim_1+ accesses onto
        // the dim_0 axis (e.g. allocatable A = transpose(reshape(...))
        // produced descriptor with stride=(1,1) and any subsequent
        // assumed-shape pass read overlapping bytes per "column").
        dest.rank = source.rank;
        dest.elem_size = source.elem_size;
        let mut running_stride: i64 = 1;
        for i in 0..source.rank as usize {
            dest.dims[i] = source.dims[i];
            dest.dims[i].stride = running_stride;
            running_stride = running_stride.saturating_mul(source.dims[i].extent().max(1));
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

    // Copy data. Source may be non-contiguous (e.g. result of
    // transpose() returns a descriptor with reversed dim strides
    // pointing at the original buffer). A flat ptr::copy of
    // total_bytes from source.base_addr would drag adjacent bytes
    // forward without honoring per-dim strides — the same class of
    // bug as the original afs_copy_array_data flat copy. Detect
    // non-contiguous and walk every multi-index column-major.
    //
    // We only treat the source as non-contiguous when at least one
    // dim's stride is negative or *strictly greater* than its
    // canonical column-major step. Positive strides smaller than
    // canonical (e.g. afs_matmul's 2x2 result emitted with
    // stride=(1,1) instead of (1,2)) describe an internally
    // inconsistent descriptor whose base_addr still points at a flat
    // contiguous buffer; walking those would re-read the same byte
    // offset twice and drop the last element. The conservative choice
    // is the flat copy that mirrors total_bytes — which the previous
    // unconditional ptr::copy did silently for both kinds of source.
    let bytes = source.total_bytes();
    if bytes > 0 && !source.base_addr.is_null() && !dest.base_addr.is_null() {
        if let Some(buf) = source_snapshot.as_ref() {
            let copy_bytes = bytes.min(dest.total_bytes()) as usize;
            unsafe {
                ptr::copy_nonoverlapping(buf.as_ptr(), dest.base_addr, copy_bytes);
            }
        } else {
            unsafe {
                copy_same_type_payload_to_contiguous(source, dest.base_addr);
            }
        }
    }
    dest.set_scalar_type_tag(source.scalar_type_tag());
    dest.set_dynamic_vtable_ptr(source.dynamic_vtable_ptr());
}

fn source_base_points_into_dest_storage(dest: &ArrayDescriptor, source: &ArrayDescriptor) -> bool {
    if !dest.is_allocated() || dest.base_addr.is_null() || source.base_addr.is_null() {
        return false;
    }
    let dest_bytes = dest.total_bytes();
    if dest_bytes <= 0 {
        return false;
    }
    let dest_start = dest.base_addr as usize;
    let dest_end = dest_start.saturating_add(dest_bytes as usize);
    let source_start = source.base_addr as usize;
    source_start >= dest_start && source_start < dest_end
}

unsafe fn copy_same_type_payload_to_contiguous(source: &ArrayDescriptor, dest_base: *mut u8) {
    let bytes = source.total_bytes();
    if bytes <= 0 || source.base_addr.is_null() || dest_base.is_null() {
        return;
    }

    let elem_size = source.elem_size;
    let mut canonical: i64 = 1;
    let mut strided = false;
    for i in 0..source.rank as usize {
        if source.dims[i].stride < 0 || source.dims[i].stride > canonical {
            strided = true;
            break;
        }
        canonical = canonical.saturating_mul(source.dims[i].extent().max(1));
    }
    if !strided {
        ptr::copy(source.base_addr, dest_base, bytes as usize);
        return;
    }

    let rank = source.rank as usize;
    let extents: Vec<i64> = (0..rank).map(|i| source.dims[i].extent()).collect();
    let strides: Vec<i64> = (0..rank).map(|i| source.dims[i].stride).collect();
    let mut idx = vec![0i64; rank];
    let total = source.total_elements();
    for k in 0..total {
        let mut src_off: i64 = 0;
        for d in 0..rank {
            src_off += idx[d] * strides[d];
        }
        src_off *= elem_size;
        let dst_off = k * elem_size;
        ptr::copy_nonoverlapping(
            source.base_addr.offset(src_off as isize),
            dest_base.offset(dst_off as isize),
            elem_size as usize,
        );
        for d in 0..rank {
            idx[d] += 1;
            if idx[d] < extents[d] {
                break;
            }
            idx[d] = 0;
        }
    }
}

unsafe fn copy_same_type_payload_between_descriptors(
    dest: &ArrayDescriptor,
    source: &ArrayDescriptor,
) {
    let total = source.total_elements();
    let elem_size = source.elem_size;
    if total <= 0 || elem_size <= 0 || source.base_addr.is_null() || dest.base_addr.is_null() {
        return;
    }
    if source.rank == 0 {
        ptr::copy(source.base_addr, dest.base_addr, elem_size as usize);
        return;
    }

    let rank = source.rank as usize;
    let extents: Vec<i64> = (0..rank).map(|i| source.dims[i].extent()).collect();
    let mut idx = vec![0i64; rank];
    for _ in 0..total {
        let mut src_off: i64 = 0;
        let mut dest_off: i64 = 0;
        for (d, &index) in idx.iter().enumerate().take(rank) {
            src_off += index * source.dims[d].stride;
            dest_off += index * dest.dims[d].stride;
        }
        ptr::copy(
            source.base_addr.offset((src_off * elem_size) as isize),
            dest.base_addr.offset((dest_off * dest.elem_size) as isize),
            elem_size as usize,
        );
        for (d, index) in idx.iter_mut().enumerate().take(rank) {
            *index += 1;
            if *index < extents[d] {
                break;
            }
            *index = 0;
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_copy_array_data_no_realloc(
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
        eprintln!("Array assignment: null descriptor");
        std::process::exit(1);
    }

    let dest_ref = unsafe { &mut *dest };
    let source_ref = unsafe { &*source };
    let shapes_match = array_assignment_shapes_conform(dest_ref, source_ref)
        && dest_ref.elem_size == source_ref.elem_size;
    let valid_source = descriptor_has_payload_or_zero_size_array(source_ref);
    let valid_dest = !dest_ref.base_addr.is_null() || descriptor_is_zero_size_array(dest_ref);

    if !shapes_match || !valid_source || !valid_dest {
        if !stat.is_null() {
            unsafe {
                *stat = 4;
            }
            return;
        }
        eprintln!(
            "Array assignment: destination shape does not conform to source \
             (dest rank={} elem={} allocated={} base_null={} extents={:?}; \
             source rank={} elem={} allocated={} base_null={} extents={:?})",
            dest_ref.rank,
            dest_ref.elem_size,
            dest_ref.is_allocated(),
            dest_ref.base_addr.is_null(),
            (0..dest_ref.rank as usize)
                .map(|i| dest_ref.dims[i].extent())
                .collect::<Vec<_>>(),
            source_ref.rank,
            source_ref.elem_size,
            source_ref.is_allocated(),
            source_ref.base_addr.is_null(),
            (0..source_ref.rank as usize)
                .map(|i| source_ref.dims[i].extent())
                .collect::<Vec<_>>()
        );
        std::process::exit(1);
    }

    unsafe {
        copy_same_type_payload_between_descriptors(dest_ref, source_ref);
    }
    dest_ref.set_scalar_type_tag(source_ref.scalar_type_tag());
    dest_ref.set_dynamic_vtable_ptr(source_ref.dynamic_vtable_ptr());

    if !stat.is_null() {
        unsafe {
            *stat = 0;
        }
    }
}

fn array_assignment_shapes_conform(dest: &ArrayDescriptor, source: &ArrayDescriptor) -> bool {
    if dest.rank != source.rank || dest.rank < 0 || dest.rank as usize > MAX_RANK {
        return false;
    }

    (0..dest.rank as usize).all(|i| {
        checked_dim_extent(dest.dims[i])
            .zip(checked_dim_extent(source.dims[i]))
            .is_some_and(|(dest_extent, source_extent)| dest_extent == source_extent)
    })
}

/// Abort when intrinsic array-assignment operands are not conformable.
///
/// This operation deliberately validates only rank and per-dimension extents:
/// semantic validation has already established assignment-compatible element
/// types, and deep-copy lowering must remain responsible for component
/// ownership rather than asking the runtime to copy descriptor payload bytes.
#[no_mangle]
pub extern "C" fn afs_check_array_assignment_conformance(
    dest: *const ArrayDescriptor,
    source: *const ArrayDescriptor,
) {
    if dest.is_null() || source.is_null() {
        eprintln!("Array assignment: null descriptor");
        std::process::exit(1);
    }

    let dest = unsafe { &*dest };
    let source = unsafe { &*source };
    if !array_assignment_shapes_conform(dest, source) {
        eprintln!(
            "Array assignment: destination shape does not conform to source \
             (dest rank={} extents={:?}; source rank={} extents={:?})",
            dest.rank,
            (0..dest.rank.max(0) as usize)
                .take(MAX_RANK)
                .filter_map(|i| checked_dim_extent(dest.dims[i]))
                .collect::<Vec<_>>(),
            source.rank,
            (0..source.rank.max(0) as usize)
                .take(MAX_RANK)
                .filter_map(|i| checked_dim_extent(source.dims[i]))
                .collect::<Vec<_>>()
        );
        std::process::exit(1);
    }
}

fn numeric_kind_elem_size(kind_tag: i32) -> Option<i64> {
    match kind_tag {
        0 => Some(1),
        1 => Some(2),
        2 | 4 => Some(4),
        3 | 5 | 6 => Some(8),
        7 => Some(16),
        _ => None,
    }
}

/// Element-converting allocatable assignment.
///
/// F2018 §10.2.1.3: when the LHS and RHS of an array assignment have
/// different numeric element types, each element is converted to the
/// LHS type. This entry point performs that conversion when the source
/// descriptor's element kind differs from the destination's.
///
/// kind_tag: 0=i8, 1=i16, 2=i32, 3=i64, 4=f32, 5=f64,
/// 6=complex(f32), 7=complex(f64)
#[no_mangle]
pub extern "C" fn afs_assign_allocatable_convert(
    dest: *mut ArrayDescriptor,
    source: *const ArrayDescriptor,
    dest_kind_tag: i32,
    src_kind_tag: i32,
) {
    if dest.is_null() || source.is_null() {
        return;
    }
    let dest_ref = unsafe { &mut *dest };
    let source_ref = unsafe { &*source };

    if !descriptor_looks_sane(dest_ref) {
        *dest_ref = ArrayDescriptor::zeroed();
    }

    if !source_ref.is_allocated()
        && source_ref.base_addr.is_null()
        && !descriptor_is_zero_size_array(source_ref)
    {
        if dest_ref.is_allocated() && !dest_ref.base_addr.is_null() {
            unsafe {
                libc_free(dest_ref.base_addr);
            }
        }
        *dest_ref = ArrayDescriptor::zeroed();
        return;
    }

    let Some(dest_elem_size) = numeric_kind_elem_size(dest_kind_tag) else {
        return;
    };

    let shapes_match = dest_ref.rank == source_ref.rank
        && dest_ref.elem_size == dest_elem_size
        && (0..dest_ref.rank as usize)
            .all(|i| dest_ref.dims[i].extent() == source_ref.dims[i].extent());

    if !shapes_match || !dest_ref.is_allocated() {
        if dest_ref.is_allocated() && !dest_ref.base_addr.is_null() {
            unsafe {
                libc_free(dest_ref.base_addr);
            }
            dest_ref.base_addr = ptr::null_mut();
            dest_ref.flags &= !DESC_ALLOCATED;
        }
        dest_ref.rank = source_ref.rank;
        dest_ref.elem_size = dest_elem_size;
        // Canonical column-major strides — see matching note in
        // afs_assign_allocatable. dest is freshly contiguous; the
        // per-dim memory step must be (1, ext_0, ext_0*ext_1, ...).
        let mut running_stride: i64 = 1;
        for i in 0..source_ref.rank as usize {
            dest_ref.dims[i] = source_ref.dims[i];
            dest_ref.dims[i].stride = running_stride;
            running_stride = running_stride.saturating_mul(source_ref.dims[i].extent().max(1));
        }
        let bytes = dest_ref.total_bytes();
        if bytes > 0 {
            let ptr = unsafe { libc_malloc(bytes as usize) };
            if ptr.is_null() {
                eprintln!("ALLOCATE (assignment): out of memory ({} bytes)", bytes);
                std::process::exit(1);
            }
            dest_ref.base_addr = ptr;
        }
        dest_ref.flags = DESC_ALLOCATED | DESC_CONTIGUOUS;
    }

    let n: usize = (0..source_ref.rank as usize)
        .map(|i| source_ref.dims[i].extent() as usize)
        .product();
    if n == 0 || source_ref.base_addr.is_null() || dest_ref.base_addr.is_null() {
        return;
    }

    let src_p = source_ref.base_addr;
    let dst_p = dest_ref.base_addr;
    let Some(src_elem_size) = numeric_kind_elem_size(src_kind_tag) else {
        return;
    };
    // Source may be non-contiguous (e.g. transpose result, section).
    // Walk each multi-index column-major and apply per-dim strides.
    // Mirror the same-class detection used in afs_assign_allocatable
    // and afs_copy_array_data: apply per-dim strides when at least
    // one stride is negative or *strictly greater* than its canonical
    // column-major step. A positive stride below canonical describes
    // a malformed descriptor (e.g. a 2x2 matmul result with
    // stride=(1,1) instead of (1,2)) whose underlying buffer is still
    // flat contiguous; walking those re-reads the same offset twice.
    let rank = source_ref.rank as usize;
    let extents: Vec<i64> = (0..rank).map(|i| source_ref.dims[i].extent()).collect();
    let raw_strides: Vec<i64> = (0..rank).map(|i| source_ref.dims[i].stride).collect();
    let mut canonical_step: i64 = 1;
    let mut canonical: Vec<i64> = Vec::with_capacity(rank);
    let mut strided = false;
    for d in 0..rank {
        canonical.push(canonical_step);
        if raw_strides[d] < 0 || raw_strides[d] > canonical_step {
            strided = true;
        }
        canonical_step = canonical_step.saturating_mul(extents[d].max(1));
    }
    let strides: &[i64] = if strided { &raw_strides } else { &canonical };
    let mut idx = vec![0i64; rank];
    for k in 0..n {
        let mut src_off_elems: i64 = 0;
        for d in 0..rank {
            src_off_elems += idx[d] * strides[d];
        }
        let src_byte_off = src_off_elems * src_elem_size;
        let (src_re_f64, src_im_f64): (f64, f64) = unsafe {
            match src_kind_tag {
                0 => (
                    *(src_p.offset(src_byte_off as isize) as *const i8) as f64,
                    0.0,
                ),
                1 => (
                    *(src_p.offset(src_byte_off as isize) as *const i16) as f64,
                    0.0,
                ),
                2 => (
                    *(src_p.offset(src_byte_off as isize) as *const i32) as f64,
                    0.0,
                ),
                3 => (
                    *(src_p.offset(src_byte_off as isize) as *const i64) as f64,
                    0.0,
                ),
                4 => (
                    *(src_p.offset(src_byte_off as isize) as *const f32) as f64,
                    0.0,
                ),
                5 => (*(src_p.offset(src_byte_off as isize) as *const f64), 0.0),
                6 => {
                    let p = src_p.offset(src_byte_off as isize) as *const f32;
                    ((*p) as f64, (*p.add(1)) as f64)
                }
                7 => {
                    let p = src_p.offset(src_byte_off as isize) as *const f64;
                    (*p, *p.add(1))
                }
                _ => return,
            }
        };
        unsafe {
            match dest_kind_tag {
                0 => *(dst_p.add(k) as *mut i8) = src_re_f64 as i8,
                1 => *(dst_p.add(2 * k) as *mut i16) = src_re_f64 as i16,
                2 => *(dst_p.add(4 * k) as *mut i32) = src_re_f64 as i32,
                3 => *(dst_p.add(8 * k) as *mut i64) = src_re_f64 as i64,
                4 => *(dst_p.add(4 * k) as *mut f32) = src_re_f64 as f32,
                5 => *(dst_p.add(8 * k) as *mut f64) = src_re_f64,
                6 => {
                    let p = dst_p.add(8 * k) as *mut f32;
                    *p = src_re_f64 as f32;
                    *p.add(1) = src_im_f64 as f32;
                }
                7 => {
                    let p = dst_p.add(16 * k) as *mut f64;
                    *p = src_re_f64;
                    *p.add(1) = src_im_f64;
                }
                _ => return,
            }
        }
        for d in 0..rank {
            idx[d] += 1;
            if idx[d] < extents[d] {
                break;
            }
            idx[d] = 0;
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
    // F2018 permits the same variable only while it is unallocated. Avoid
    // creating aliased Rust references even if an invalid caller violates it.
    if from.is_null() || to.is_null() || std::ptr::eq(from, to) {
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
    from_desc.clear_dynamic_type_metadata();
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

fn descriptor_byte_span(desc: &ArrayDescriptor) -> Option<(i128, i128)> {
    if desc.base_addr.is_null()
        || desc.elem_size <= 0
        || desc.rank < 0
        || desc.rank as usize > MAX_RANK
    {
        return None;
    }

    let mut min_element_offset = 0_i128;
    let mut max_element_offset = 0_i128;
    for dim in desc.dims.iter().take(desc.rank as usize) {
        let extent = dim.extent();
        if extent <= 0 {
            return None;
        }
        let last_offset = i128::from(extent - 1).saturating_mul(i128::from(dim.stride));
        if last_offset < 0 {
            min_element_offset = min_element_offset.saturating_add(last_offset);
        } else {
            max_element_offset = max_element_offset.saturating_add(last_offset);
        }
    }

    let elem_size = i128::from(desc.elem_size);
    let base = desc.base_addr as usize as i128;
    let low = base.saturating_add(min_element_offset.saturating_mul(elem_size));
    let high = base
        .saturating_add(max_element_offset.saturating_mul(elem_size))
        .saturating_add(elem_size);
    Some((low.min(high), low.max(high)))
}

/// Return nonzero when two descriptors may address any common storage.
///
/// The comparison is deliberately conservative for strided sections: it
/// compares the byte ranges spanning their first and last reachable elements.
/// False positives only force an assignment snapshot; false negatives would
/// permit a destination reallocation to invalidate its source.
#[no_mangle]
pub extern "C" fn afs_descriptors_overlap(
    left: *const ArrayDescriptor,
    right: *const ArrayDescriptor,
) -> i32 {
    if left.is_null() || right.is_null() {
        return 0;
    }
    let (Some((left_low, left_high)), Some((right_low, right_high))) = (
        descriptor_byte_span(unsafe { &*left }),
        descriptor_byte_span(unsafe { &*right }),
    ) else {
        return 0;
    };
    i32::from(left_low < right_high && right_low < left_high)
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
    result.flags = DESC_CONTIGUOUS | (source.flags & DESC_TYPE_TAG_MASK);
    result.vtable = source.vtable;
    // Don't set DESC_ALLOCATED — section doesn't own the data.

    // Compute base address offset and new dims.
    //
    // The descriptor convention here is that `dim[k].stride` already encodes
    // the *memory step in elements* between adjacent positions along dim k —
    // see materialize_array_descriptor_for_info in src/ir/lower.rs which
    // builds dim[k].stride = product(extents[0..k]) for a contiguous array.
    // So byte_offset and surviving-dim memory strides are computed directly
    // from src_dim.stride; no extra column-major multiplier is needed.
    //
    // SectionSpec.stride == 0 is a sentinel for *rank-reducing* scalar
    // selection (e.g. the `1` in `y(1,:)`). Those dims contribute to the
    // base offset but do NOT appear in the result descriptor.
    let mut byte_offset: i64 = 0;
    let mut result_rank: i32 = 0;

    for (i, spec) in specs_slice.iter().enumerate() {
        let src_dim = &source.dims[i];

        // Offset from source lower bound to section start.
        let start_idx = spec.start - src_dim.lower_bound;
        byte_offset += start_idx * src_dim.stride * source.elem_size;

        if spec.stride != 0 {
            // Slice: keep this dim. Extent = max(0, (end - start) / stride + 1).
            // For negative strides, start > end and (end-start)/stride is positive.
            // For a positive stride where start > end, result is empty (extent 0).
            let extent = if (spec.stride > 0 && spec.start > spec.end)
                || (spec.stride < 0 && spec.start < spec.end)
            {
                0 // empty section
            } else {
                (spec.end - spec.start) / spec.stride + 1
            };
            result.dims[result_rank as usize] = DimDescriptor {
                lower_bound: 1, // sections are always 1-based
                upper_bound: extent,
                stride: src_dim.stride * spec.stride,
            };
            result_rank += 1;
        }
    }

    result.rank = result_rank;

    // Result base_addr = source base_addr + offset.
    if !source.base_addr.is_null() {
        // byte_offset can be negative for negative-stride sections.
        result.base_addr = unsafe { source.base_addr.offset(byte_offset as isize) };
    } else {
        result.base_addr = ptr::null_mut();
    }

    // Check contiguity: contiguous iff every surviving dim has stride 1.
    let is_contig = (0..result_rank as usize).all(|i| result.dims[i].stride == 1);
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

fn set_rank2_contiguous_shape(desc: &mut ArrayDescriptor, dim0: usize, dim1: usize) {
    desc.rank = 2;
    desc.dims[0] = DimDescriptor {
        lower_bound: 1,
        upper_bound: dim0 as i64,
        stride: 1,
    };
    desc.dims[1] = DimDescriptor {
        lower_bound: 1,
        upper_bound: dim1 as i64,
        stride: dim0.max(1) as i64,
    };
}

#[derive(Clone, Copy)]
struct MatmulShape {
    m: usize,
    k: usize,
    n: usize,
    result_rank: i32,
}

fn matmul_shape(a: &ArrayDescriptor, b: &ArrayDescriptor) -> Option<MatmulShape> {
    match (a.rank, b.rank) {
        (2, 2) => {
            let k = a.dims[1].extent() as usize;
            if b.dims[0].extent() as usize != k {
                return None;
            }
            Some(MatmulShape {
                m: a.dims[0].extent() as usize,
                k,
                n: b.dims[1].extent() as usize,
                result_rank: 2,
            })
        }
        (2, 1) => {
            let k = a.dims[1].extent() as usize;
            if b.dims[0].extent() as usize != k {
                return None;
            }
            Some(MatmulShape {
                m: a.dims[0].extent() as usize,
                k,
                n: 1,
                result_rank: 1,
            })
        }
        (1, 2) => {
            let k = a.dims[0].extent() as usize;
            if b.dims[0].extent() as usize != k {
                return None;
            }
            Some(MatmulShape {
                m: 1,
                k,
                n: b.dims[1].extent() as usize,
                result_rank: 1,
            })
        }
        _ => None,
    }
}

fn allocate_matmul_result(result: *mut ArrayDescriptor, elem_size: i64, shape: MatmulShape) {
    let len = if shape.result_rank == 1 {
        shape.m.max(shape.n)
    } else {
        shape.m * shape.n
    };
    afs_allocate_1d(result, elem_size, len as i64);
    if shape.result_rank == 2 {
        let res = unsafe { &mut *result };
        set_rank2_contiguous_shape(res, shape.m, shape.n);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn i32_descriptor(data: &mut [i32], extents: &[i64]) -> ArrayDescriptor {
        let mut desc = ArrayDescriptor::zeroed();
        desc.base_addr = data.as_mut_ptr() as *mut u8;
        desc.elem_size = 4;
        desc.rank = extents.len() as i32;
        let mut stride = 1;
        for (i, extent) in extents.iter().copied().enumerate() {
            desc.dims[i] = DimDescriptor {
                lower_bound: 1,
                upper_bound: extent,
                stride,
            };
            stride *= extent.max(0);
        }
        desc
    }

    fn f32_descriptor(data: &mut [f32], extents: &[i64]) -> ArrayDescriptor {
        let mut desc = ArrayDescriptor::zeroed();
        desc.base_addr = data.as_mut_ptr() as *mut u8;
        desc.elem_size = 4;
        desc.rank = extents.len() as i32;
        let mut stride = 1;
        for (i, extent) in extents.iter().copied().enumerate() {
            desc.dims[i] = DimDescriptor {
                lower_bound: 1,
                upper_bound: extent,
                stride,
            };
            stride *= extent.max(0);
        }
        desc
    }

    fn strided_descriptor<T>(
        data: &mut [T],
        base_index: usize,
        extents: &[i64],
        strides: &[i64],
    ) -> ArrayDescriptor {
        assert_eq!(extents.len(), strides.len());
        assert!(base_index < data.len());
        let mut desc = ArrayDescriptor::zeroed();
        desc.base_addr = unsafe { data.as_mut_ptr().add(base_index) as *mut u8 };
        desc.elem_size = std::mem::size_of::<T>() as i64;
        desc.rank = extents.len() as i32;
        for (i, (&extent, &stride)) in extents.iter().zip(strides).enumerate() {
            desc.dims[i] = DimDescriptor {
                lower_bound: 1,
                upper_bound: extent,
                stride,
            };
        }
        desc
    }

    fn assert_rank_two_scalar_reductions(
        real8: &ArrayDescriptor,
        real4: &ArrayDescriptor,
        ints: &ArrayDescriptor,
        mask: &ArrayDescriptor,
        norm2: f64,
        sum: f64,
        product: f64,
        minval: f64,
        maxval: f64,
    ) {
        assert!((afs_array_norm2_real8(real8) - norm2).abs() < 1.0e-12);
        assert!((f64::from(afs_array_norm2_real4(real4)) - norm2).abs() < 1.0e-5);

        assert_eq!(afs_array_sum_real8_mask(real8, mask), sum);
        assert_eq!(afs_array_sum_real8_mask(real4, mask), sum);
        assert_eq!(afs_array_sum_int_mask(ints, mask), sum as i64);

        assert_eq!(afs_array_product_real8_mask(real8, mask), product);
        assert_eq!(afs_array_product_real8_mask(real4, mask), product);
        assert_eq!(afs_array_product_int_mask(ints, mask), product as i64);

        assert_eq!(afs_array_minval_real8_mask(real8, mask), minval);
        assert_eq!(afs_array_minval_real8_mask(real4, mask), minval);
        assert_eq!(afs_array_minval_int_mask(ints, mask), minval as i64);

        assert_eq!(afs_array_maxval_real8_mask(real8, mask), maxval);
        assert_eq!(afs_array_maxval_real8_mask(real4, mask), maxval);
        assert_eq!(afs_array_maxval_int_mask(ints, mask), maxval as i64);
    }

    #[test]
    fn scalar_reductions_walk_contiguous_rank_two_descriptors() {
        let mut real8_data = [2.0_f64, 3.0, 5.0, 7.0];
        let mut real4_data = [2.0_f32, 3.0, 5.0, 7.0];
        let mut int_data = [2_i32, 3, 5, 7];
        let mut mask_data = [1_u32, 0, 1, 1];
        let extents = [2, 2];
        let strides = [1, 2];
        let real8 = strided_descriptor(&mut real8_data, 0, &extents, &strides);
        let real4 = strided_descriptor(&mut real4_data, 0, &extents, &strides);
        let ints = strided_descriptor(&mut int_data, 0, &extents, &strides);
        let mask = strided_descriptor(&mut mask_data, 0, &extents, &strides);

        assert_rank_two_scalar_reductions(
            &real8,
            &real4,
            &ints,
            &mask,
            87.0_f64.sqrt(),
            14.0,
            70.0,
            2.0,
            7.0,
        );
    }

    #[test]
    fn scalar_reductions_walk_noncontiguous_rank_two_descriptors() {
        let mut real8_data = [101.0_f64; 12];
        let mut real4_data = [101.0_f32; 12];
        let mut int_data = [101_i32; 12];
        for (index, value) in [(0, 2), (2, 3), (5, 5), (7, 7)] {
            real8_data[index] = f64::from(value);
            real4_data[index] = value as f32;
            int_data[index] = value;
        }
        let mut mask_data = [0_u32; 12];
        for index in [0, 4, 7] {
            mask_data[index] = 1;
        }
        let extents = [2, 2];
        let real8 = strided_descriptor(&mut real8_data, 0, &extents, &[2, 5]);
        let real4 = strided_descriptor(&mut real4_data, 0, &extents, &[2, 5]);
        let ints = strided_descriptor(&mut int_data, 0, &extents, &[2, 5]);
        let mask = strided_descriptor(&mut mask_data, 0, &extents, &[3, 4]);

        assert_rank_two_scalar_reductions(
            &real8,
            &real4,
            &ints,
            &mask,
            87.0_f64.sqrt(),
            14.0,
            70.0,
            2.0,
            7.0,
        );
    }

    #[test]
    fn scalar_reductions_preserve_negative_rank_two_strides() {
        let mut real8_data = [101.0_f64; 16];
        let mut real4_data = [101.0_f32; 16];
        let mut int_data = [101_i32; 16];
        for (index, value) in [(8, 2), (6, 3), (3, 5), (1, 7)] {
            real8_data[index] = f64::from(value);
            real4_data[index] = value as f32;
            int_data[index] = value;
        }
        let mut mask_data = [0_u32; 16];
        for index in [8, 4, 1] {
            mask_data[index] = 1;
        }
        let extents = [2, 2];
        let real8 = strided_descriptor(&mut real8_data, 8, &extents, &[-2, -5]);
        let real4 = strided_descriptor(&mut real4_data, 8, &extents, &[-2, -5]);
        let ints = strided_descriptor(&mut int_data, 8, &extents, &[-2, -5]);
        let mask = strided_descriptor(&mut mask_data, 8, &extents, &[-3, -4]);

        assert_rank_two_scalar_reductions(
            &real8,
            &real4,
            &ints,
            &mask,
            87.0_f64.sqrt(),
            14.0,
            70.0,
            2.0,
            7.0,
        );
    }

    #[test]
    fn scalar_reductions_return_identities_for_zero_extent_descriptors() {
        let mut real8_data = [9.0_f64];
        let mut real4_data = [9.0_f32];
        let mut int_data = [9_i32];
        let mut mask_data = [1_u32];
        let extents = [2, 0];
        let strides = [1, 2];
        let real8 = strided_descriptor(&mut real8_data, 0, &extents, &strides);
        let real4 = strided_descriptor(&mut real4_data, 0, &extents, &strides);
        let ints = strided_descriptor(&mut int_data, 0, &extents, &strides);
        let mask = strided_descriptor(&mut mask_data, 0, &extents, &strides);

        assert_eq!(afs_array_norm2_real8(&real8), 0.0);
        assert_eq!(afs_array_norm2_real4(&real4), 0.0);
        assert_eq!(afs_array_sum_real8_mask(&real8, &mask), 0.0);
        assert_eq!(afs_array_sum_real8_mask(&real4, &mask), 0.0);
        assert_eq!(afs_array_sum_int_mask(&ints, &mask), 0);
        assert_eq!(afs_array_product_real8_mask(&real8, &mask), 1.0);
        assert_eq!(afs_array_product_real8_mask(&real4, &mask), 1.0);
        assert_eq!(afs_array_product_int_mask(&ints, &mask), 1);
        assert_eq!(afs_array_maxval_real8_mask(&real8, &mask), -f64::MAX);
        assert_eq!(
            afs_array_maxval_real8_mask(&real4, &mask),
            -(f32::MAX as f64)
        );
        assert_eq!(afs_array_maxval_int_mask(&ints, &mask), i32::MIN as i64);
        assert_eq!(afs_array_minval_real8_mask(&real8, &mask), f64::MAX);
        assert_eq!(afs_array_minval_real8_mask(&real4, &mask), f32::MAX as f64);
        assert_eq!(afs_array_minval_int_mask(&ints, &mask), i32::MAX as i64);
    }

    #[test]
    fn array_bounds_canonicalize_only_the_zero_extent_dimension() {
        let mut desc = ArrayDescriptor::zeroed();
        desc.rank = 2;
        desc.dims[0] = DimDescriptor {
            lower_bound: -4,
            upper_bound: -5,
            stride: 1,
        };
        desc.dims[1] = DimDescriptor {
            lower_bound: 7,
            upper_bound: 9,
            stride: 1,
        };

        assert_eq!(afs_array_lbound(&desc, 1), 1);
        assert_eq!(afs_array_ubound(&desc, 1), 0);
        assert_eq!(afs_array_lbound(&desc, 2), 7);
        assert_eq!(afs_array_ubound(&desc, 2), 9);
    }

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
    fn allocation_overflow_preserves_descriptor() {
        let mut desc = ArrayDescriptor::zeroed();
        desc.elem_size = 17;
        desc.dims[0] = DimDescriptor {
            lower_bound: 7,
            upper_bound: 9,
            stride: 3,
        };
        let dims = [DimDescriptor {
            lower_bound: 1,
            upper_bound: i64::MAX / 4 + 1,
            stride: 1,
        }];
        let mut stat = -1;

        afs_allocate_array(&mut desc, 4, 1, dims.as_ptr(), &mut stat);

        assert_ne!(stat, 0);
        assert!(!desc.is_allocated());
        assert!(desc.base_addr.is_null());
        assert_eq!(desc.elem_size, 17);
        assert_eq!(desc.rank, 0);
        assert_eq!(desc.dims[0].lower_bound, 7);
        assert_eq!(desc.dims[0].upper_bound, 9);
        assert_eq!(desc.dims[0].stride, 3);
    }

    #[test]
    fn allocation_rejects_extent_stride_and_rank_overflow() {
        let cases = [
            (
                1,
                vec![DimDescriptor {
                    lower_bound: i64::MIN,
                    upper_bound: i64::MAX,
                    stride: 1,
                }],
            ),
            (
                2,
                vec![
                    DimDescriptor {
                        lower_bound: 1,
                        upper_bound: i64::MAX,
                        stride: 1,
                    },
                    DimDescriptor {
                        lower_bound: 1,
                        upper_bound: 2,
                        stride: 1,
                    },
                ],
            ),
            (16, vec![DimDescriptor::default(); 16]),
            (-1, vec![]),
        ];

        for (rank, dims) in cases {
            let mut desc = ArrayDescriptor::zeroed();
            let mut stat = -1;
            let dims_ptr = if dims.is_empty() {
                ptr::null()
            } else {
                dims.as_ptr()
            };

            afs_allocate_array(&mut desc, 4, rank, dims_ptr, &mut stat);

            assert_ne!(stat, 0, "rank {rank} unexpectedly succeeded");
            assert!(!desc.is_allocated(), "rank {rank} marked allocated");
            assert!(desc.base_addr.is_null(), "rank {rank} published storage");
            assert_eq!(desc.rank, 0, "rank {rank} mutated the descriptor");
        }
    }

    #[test]
    fn deallocate_unallocated_sets_stat() {
        let mut desc = ArrayDescriptor::zeroed();
        let mut stat = -1;
        afs_deallocate_array(&mut desc, &mut stat);
        assert_ne!(stat, 0);
        assert!(!desc.is_allocated());
        assert!(desc.base_addr.is_null());
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
        // Bounds carry over from source. Strides are canonical
        // column-major (stride[0]=1, stride[k]=Π extent[0..k]) — see
        // matching note in afs_allocate_array. The previous flat-1
        // strides made downstream `afs_create_section` compute
        // colliding byte offsets for any rank-2 reshape.
        assert_eq!(dest.dims[0].lower_bound, -2);
        assert_eq!(dest.dims[0].upper_bound, 1);
        assert_eq!(dest.dims[0].stride, 1);
        assert_eq!(dest.dims[1].lower_bound, 4);
        assert_eq!(dest.dims[1].upper_bound, 6);
        // dim[1].stride = extent[0] = 1-(-2)+1 = 4
        assert_eq!(dest.dims[1].stride, 4);

        afs_deallocate_array(&mut dest, ptr::null_mut());
    }

    #[test]
    fn matmul_and_transpose_publish_rank2_column_major_strides() {
        let mut data = [1.0_f32, 2.0, 3.0, 4.0];
        let source = f32_descriptor(&mut data, &[2, 2]);

        let mut transposed = ArrayDescriptor::zeroed();
        afs_transpose_real8(&source, &mut transposed);
        assert_eq!(transposed.rank, 2);
        assert_eq!(transposed.dims[0].stride, 1);
        assert_eq!(transposed.dims[1].stride, 2);
        afs_deallocate_array(&mut transposed, ptr::null_mut());

        let mut product = ArrayDescriptor::zeroed();
        afs_matmul_real8(&source, &source, &mut product);
        assert_eq!(product.rank, 2);
        assert_eq!(product.dims[0].stride, 1);
        assert_eq!(product.dims[1].stride, 2);
        afs_deallocate_array(&mut product, ptr::null_mut());
    }

    #[test]
    fn matmul_matrix_vector_and_vector_matrix_publish_rank1_shape() {
        let mut matrix_data = [
            9.0_f32, 4.0, 0.0, 4.0, 0.0, 7.0, 8.0, 0.0, 0.0, 0.0, -1.0, 5.0, 0.0, 0.0, 8.0, 6.0,
            -3.0, 0.0, 0.0, 0.0,
        ];
        let mut col_vector_data = [1.0_f32; 5];
        let matrix = f32_descriptor(&mut matrix_data, &[4, 5]);
        let col_vector = f32_descriptor(&mut col_vector_data, &[5]);

        let mut product = ArrayDescriptor::zeroed();
        afs_matmul_real8(&matrix, &col_vector, &mut product);
        assert_eq!(product.rank, 1);
        assert_eq!(product.dims[0].extent(), 4);
        let got =
            unsafe { core::slice::from_raw_parts(product.base_addr as *const f32, 4).to_vec() };
        assert_eq!(got, vec![6.0, 11.0, 15.0, 15.0]);
        afs_deallocate_array(&mut product, ptr::null_mut());

        let mut row_vector_data = [1.0_f32; 4];
        let row_vector = f32_descriptor(&mut row_vector_data, &[4]);
        afs_matmul_real8(&row_vector, &matrix, &mut product);
        assert_eq!(product.rank, 1);
        assert_eq!(product.dims[0].extent(), 5);
        let got =
            unsafe { core::slice::from_raw_parts(product.base_addr as *const f32, 5).to_vec() };
        assert_eq!(got, vec![17.0, 15.0, 4.0, 14.0, -3.0]);
        afs_deallocate_array(&mut product, ptr::null_mut());
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
    fn copy_array_data_accepts_zero_byte_source_descriptors() {
        let empty_dims = [DimDescriptor {
            lower_bound: -4,
            upper_bound: -5,
            stride: 1,
        }];
        let nonempty_dims = [DimDescriptor {
            lower_bound: 3,
            upper_bound: 5,
            stride: 1,
        }];

        for (elem_size, dims) in [(4, &empty_dims), (0, &nonempty_dims)] {
            let mut source = ArrayDescriptor::zeroed();
            let mut dest = ArrayDescriptor::zeroed();
            let mut stat = -1;
            afs_allocate_array(&mut source, elem_size, 1, dims.as_ptr(), ptr::null_mut());
            afs_allocate_array(&mut dest, elem_size, 1, dims.as_ptr(), ptr::null_mut());
            assert!(source.is_allocated());
            assert!(source.base_addr.is_null());
            assert_eq!(source.total_bytes(), 0);

            afs_copy_array_data(&mut dest, &source, &mut stat);

            assert_eq!(stat, 0);
            assert!(dest.is_allocated());
            assert!(dest.base_addr.is_null());
            afs_deallocate_array(&mut source, ptr::null_mut());
            afs_deallocate_array(&mut dest, ptr::null_mut());
        }

        let mut source = ArrayDescriptor::zeroed();
        source.elem_size = 4;
        source.rank = 1;
        source.flags = DESC_CONTIGUOUS;
        source.dims[0] = empty_dims[0];
        let mut dest = ArrayDescriptor::zeroed();
        let mut stat = -1;
        afs_allocate_array(&mut dest, 4, 1, empty_dims.as_ptr(), ptr::null_mut());

        afs_copy_array_data(&mut dest, &source, &mut stat);

        assert_eq!(stat, 0);
        assert!(dest.is_allocated());
        assert!(dest.base_addr.is_null());
        afs_deallocate_array(&mut dest, ptr::null_mut());
    }

    #[test]
    fn copy_array_data_rejects_null_nonzero_source_payload() {
        let dims = [DimDescriptor {
            lower_bound: 1,
            upper_bound: 2,
            stride: 1,
        }];
        let mut source = ArrayDescriptor::zeroed();
        source.elem_size = 4;
        source.rank = 1;
        source.flags = DESC_CONTIGUOUS;
        source.dims[0] = dims[0];
        let mut dest = ArrayDescriptor::zeroed();
        let mut stat = -1;
        afs_allocate_array(&mut dest, 4, 1, dims.as_ptr(), ptr::null_mut());

        afs_copy_array_data(&mut dest, &source, &mut stat);

        assert_eq!(stat, 4);
        assert!(!dest.is_allocated());
        assert!(dest.base_addr.is_null());
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
    fn move_alloc_preserves_scalar_type_tag() {
        let mut from = ArrayDescriptor::zeroed();
        let mut to = ArrayDescriptor::zeroed();
        afs_allocate_array(&mut from, 8, 0, ptr::null(), ptr::null_mut());
        from.set_scalar_type_tag(77);

        afs_move_alloc(&mut from, &mut to);
        assert_eq!(to.scalar_type_tag(), 77);
        assert_eq!(from.scalar_type_tag(), 0);

        afs_deallocate_array(&mut to, ptr::null_mut());
    }

    #[test]
    fn move_alloc_same_descriptor_is_a_safe_noop() {
        let mut desc = ArrayDescriptor::zeroed();
        afs_allocate_1d(&mut desc, 4, 3);
        let original_base = desc.base_addr;
        let same = &mut desc as *mut ArrayDescriptor;

        afs_move_alloc(same, same);

        assert!(desc.is_allocated());
        assert_eq!(desc.base_addr, original_base);
        assert_eq!(desc.total_elements(), 3);
        afs_deallocate_array(&mut desc, ptr::null_mut());
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
    fn reshape_order_permutation_reorders_result_slots() {
        let mut source_data = [1, 2, 3, 4, 5, 6];
        let mut shape_data = [3, 2];
        let mut order_data = [2, 1];
        let source = i32_descriptor(&mut source_data, &[6]);
        let shape = i32_descriptor(&mut shape_data, &[2]);
        let order = i32_descriptor(&mut order_data, &[2]);
        let mut result = ArrayDescriptor::zeroed();

        afs_array_reshape(&source, &shape, &order, ptr::null(), &mut result);

        assert_eq!(result.rank, 2);
        assert_eq!(result.dims[0].extent(), 3);
        assert_eq!(result.dims[1].extent(), 2);
        let got =
            unsafe { core::slice::from_raw_parts(result.base_addr as *const i32, 6).to_vec() };
        assert_eq!(got, vec![1, 3, 5, 2, 4, 6]);
        afs_deallocate_array(&mut result, ptr::null_mut());
    }

    #[test]
    fn reshape_invalid_order_falls_back_to_identity() {
        let mut source_data: [i32; 24] = core::array::from_fn(|i| (i + 1) as i32);
        let mut shape_data = [3, 2, 4];
        let mut order_data = [1_808_443_440, 1, 3];
        let source = i32_descriptor(&mut source_data, &[24]);
        let shape = i32_descriptor(&mut shape_data, &[3]);
        let order = i32_descriptor(&mut order_data, &[3]);
        let mut result = ArrayDescriptor::zeroed();

        afs_array_reshape(&source, &shape, &order, ptr::null(), &mut result);

        assert_eq!(result.rank, 3);
        let got =
            unsafe { core::slice::from_raw_parts(result.base_addr as *const i32, 6).to_vec() };
        assert_eq!(got, vec![1, 2, 3, 4, 5, 6]);
        afs_deallocate_array(&mut result, ptr::null_mut());
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
    fn character_assignment_reallocates_only_when_allocation_does_not_conform() {
        let dest_dims = [
            DimDescriptor {
                lower_bound: 0,
                upper_bound: 1,
                stride: 1,
            },
            DimDescriptor {
                lower_bound: -1,
                upper_bound: 0,
                stride: 2,
            },
        ];
        let source_dims = [
            DimDescriptor {
                lower_bound: 5,
                upper_bound: 6,
                stride: 1,
            },
            DimDescriptor {
                lower_bound: 7,
                upper_bound: 8,
                stride: 2,
            },
        ];
        let mut dest = ArrayDescriptor::zeroed();
        let mut source = ArrayDescriptor::zeroed();
        afs_allocate_array(&mut dest, 3, 2, dest_dims.as_ptr(), ptr::null_mut());
        afs_allocate_array(&mut source, 3, 2, source_dims.as_ptr(), ptr::null_mut());

        assert_eq!(
            afs_char_array_assignment_requires_reallocation(&dest, &source, 3),
            0
        );
        assert_eq!(
            afs_char_array_assignment_requires_reallocation(&dest, &source, 4),
            1
        );

        source.dims[1].upper_bound = 9;
        assert_eq!(
            afs_char_array_assignment_requires_reallocation(&dest, &source, 3),
            1
        );
        let unallocated = ArrayDescriptor::zeroed();
        assert_eq!(
            afs_char_array_assignment_requires_reallocation(&unallocated, &source, 3),
            1
        );

        let mut zero_len_dest = ArrayDescriptor::zeroed();
        let mut zero_len_source = ArrayDescriptor::zeroed();
        afs_allocate_array(
            &mut zero_len_dest,
            0,
            2,
            dest_dims.as_ptr(),
            ptr::null_mut(),
        );
        afs_allocate_array(
            &mut zero_len_source,
            0,
            2,
            source_dims.as_ptr(),
            ptr::null_mut(),
        );
        let zero_len_requires_reallocation =
            afs_char_array_assignment_requires_reallocation(&zero_len_dest, &zero_len_source, 0);
        assert_eq!(zero_len_requires_reallocation, 0);

        let empty_dest_dims = [DimDescriptor {
            lower_bound: -4,
            upper_bound: -5,
            stride: 1,
        }];
        let empty_source_dims = [DimDescriptor {
            lower_bound: 2,
            upper_bound: 1,
            stride: 1,
        }];
        let mut empty_dest = ArrayDescriptor::zeroed();
        let mut empty_source = ArrayDescriptor::zeroed();
        afs_allocate_array(
            &mut empty_dest,
            3,
            1,
            empty_dest_dims.as_ptr(),
            ptr::null_mut(),
        );
        afs_allocate_array(
            &mut empty_source,
            3,
            1,
            empty_source_dims.as_ptr(),
            ptr::null_mut(),
        );
        assert_eq!(
            afs_char_array_assignment_requires_reallocation(&empty_dest, &empty_source, 3),
            0
        );
        empty_source.dims[0].upper_bound = 2;
        assert_eq!(
            afs_char_array_assignment_requires_reallocation(&empty_dest, &empty_source, 3),
            1
        );

        let mut empty_zero_len_dest = ArrayDescriptor::zeroed();
        let mut empty_zero_len_source = ArrayDescriptor::zeroed();
        afs_allocate_array(
            &mut empty_zero_len_dest,
            0,
            1,
            empty_dest_dims.as_ptr(),
            ptr::null_mut(),
        );
        afs_allocate_array(
            &mut empty_zero_len_source,
            0,
            1,
            empty_source_dims.as_ptr(),
            ptr::null_mut(),
        );
        assert_eq!(
            afs_char_array_assignment_requires_reallocation(
                &empty_zero_len_dest,
                &empty_zero_len_source,
                0,
            ),
            0
        );

        afs_deallocate_array(&mut empty_zero_len_source, ptr::null_mut());
        afs_deallocate_array(&mut empty_zero_len_dest, ptr::null_mut());
        afs_deallocate_array(&mut empty_source, ptr::null_mut());
        afs_deallocate_array(&mut empty_dest, ptr::null_mut());
        afs_deallocate_array(&mut zero_len_source, ptr::null_mut());
        afs_deallocate_array(&mut zero_len_dest, ptr::null_mut());
        afs_deallocate_array(&mut source, ptr::null_mut());
        afs_deallocate_array(&mut dest, ptr::null_mut());
    }

    #[test]
    fn assign_allocatable_preserves_scalar_type_tag() {
        let mut source = ArrayDescriptor::zeroed();
        let mut dest = ArrayDescriptor::zeroed();

        afs_allocate_array(&mut source, 8, 0, ptr::null(), ptr::null_mut());
        source.set_scalar_type_tag(42);

        afs_assign_allocatable(&mut dest, &source);
        assert_eq!(dest.scalar_type_tag(), 42);

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
    fn assign_allocatable_preserves_nonowning_zero_size_source_shape() {
        let mut source = ArrayDescriptor::zeroed();
        source.elem_size = 8;
        source.rank = 1;
        source.flags = DESC_CONTIGUOUS;
        source.dims[0] = DimDescriptor {
            lower_bound: 1,
            upper_bound: 0,
            stride: 1,
        };
        let mut dest = ArrayDescriptor::zeroed();

        afs_assign_allocatable(&mut dest, &source);

        assert!(dest.is_allocated());
        assert!(dest.base_addr.is_null());
        assert_eq!(dest.rank, 1);
        assert_eq!(dest.elem_size, 8);
        assert_eq!(dest.total_elements(), 0);
        assert_eq!(afs_array_size(&dest), 0);
    }

    #[test]
    fn dim_reductions_allocate_unreduced_shape_for_empty_reduced_extent() {
        let mut source = ArrayDescriptor::zeroed();
        source.elem_size = 8;
        source.rank = 2;
        source.flags = DESC_ALLOCATED | DESC_CONTIGUOUS;
        source.dims[0] = DimDescriptor {
            lower_bound: 1,
            upper_bound: 6,
            stride: 1,
        };
        source.dims[1] = DimDescriptor {
            lower_bound: 1,
            upper_bound: 0,
            stride: 6,
        };
        assert_eq!(source.total_elements(), 0);
        assert!(source.base_addr.is_null());

        let mut max_result = ArrayDescriptor::zeroed();
        afs_array_maxval_real8_dim(&source, 2, &mut max_result);
        assert!(max_result.is_allocated());
        assert_eq!(max_result.rank, 1);
        assert_eq!(max_result.dims[0].extent(), 6);
        assert!(!max_result.base_addr.is_null());
        let max_values =
            unsafe { core::slice::from_raw_parts(max_result.base_addr as *const f64, 6) };
        assert!(max_values.iter().all(|value| *value == -f64::MAX));

        let mut min_result = ArrayDescriptor::zeroed();
        afs_array_minval_real8_dim(&source, 2, &mut min_result);
        assert!(min_result.is_allocated());
        assert_eq!(min_result.rank, 1);
        assert_eq!(min_result.dims[0].extent(), 6);
        assert!(!min_result.base_addr.is_null());
        let min_values =
            unsafe { core::slice::from_raw_parts(min_result.base_addr as *const f64, 6) };
        assert!(min_values.iter().all(|value| *value == f64::MAX));

        let mut sum_result = ArrayDescriptor::zeroed();
        afs_array_sum_real8_dim(&source, 2, &mut sum_result);
        assert!(sum_result.is_allocated());
        assert_eq!(sum_result.rank, 1);
        assert_eq!(sum_result.dims[0].extent(), 6);
        assert!(!sum_result.base_addr.is_null());
        let sum_values =
            unsafe { core::slice::from_raw_parts(sum_result.base_addr as *const f64, 6) };
        assert!(sum_values.iter().all(|value| *value == 0.0));

        afs_deallocate_array(&mut max_result, ptr::null_mut());
        afs_deallocate_array(&mut min_result, ptr::null_mut());
        afs_deallocate_array(&mut sum_result, ptr::null_mut());
    }

    #[test]
    fn location_dim_reductions_return_indices_along_reduced_extent() {
        let mut source = ArrayDescriptor::zeroed();
        let dims = [
            DimDescriptor {
                lower_bound: 1,
                upper_bound: 3,
                stride: 1,
            },
            DimDescriptor {
                lower_bound: 1,
                upper_bound: 4,
                stride: 3,
            },
        ];
        afs_allocate_array(&mut source, 8, 2, dims.as_ptr(), ptr::null_mut());
        assert!(source.is_allocated());

        let values = unsafe { core::slice::from_raw_parts_mut(source.base_addr as *mut f64, 12) };
        values.copy_from_slice(&[
            1.0, 8.0, 0.0, -5.0, 6.0, 7.0, 3.0, -2.0, 11.0, 9.0, 1.0, 4.0,
        ]);

        let mut max_result = ArrayDescriptor::zeroed();
        afs_array_maxloc_real8_dim(&source, 2, &mut max_result);
        assert!(max_result.is_allocated());
        assert_eq!(max_result.rank, 1);
        assert_eq!(max_result.elem_size, 4);
        assert_eq!(max_result.dims[0].extent(), 3);
        let max_values =
            unsafe { core::slice::from_raw_parts(max_result.base_addr as *const i32, 3) };
        assert_eq!(max_values, &[4, 1, 3]);

        let mut min_result = ArrayDescriptor::zeroed();
        afs_array_minloc_real8_dim(&source, 2, &mut min_result);
        assert!(min_result.is_allocated());
        assert_eq!(min_result.rank, 1);
        assert_eq!(min_result.elem_size, 4);
        assert_eq!(min_result.dims[0].extent(), 3);
        let min_values =
            unsafe { core::slice::from_raw_parts(min_result.base_addr as *const i32, 3) };
        assert_eq!(min_values, &[2, 3, 1]);

        afs_deallocate_array(&mut source, ptr::null_mut());
        afs_deallocate_array(&mut max_result, ptr::null_mut());
        afs_deallocate_array(&mut min_result, ptr::null_mut());
    }

    #[test]
    fn copy_array_data_no_realloc_preserves_destination_ownership() {
        let dims = [
            DimDescriptor {
                lower_bound: 1,
                upper_bound: 2,
                stride: 1,
            },
            DimDescriptor {
                lower_bound: 1,
                upper_bound: 2,
                stride: 2,
            },
        ];
        let mut dest = ArrayDescriptor::zeroed();
        let mut source = ArrayDescriptor::zeroed();
        afs_allocate_array(&mut dest, 8, 2, dims.as_ptr(), ptr::null_mut());
        afs_allocate_array(&mut source, 8, 2, dims.as_ptr(), ptr::null_mut());
        let dest_base = dest.base_addr;

        let source_values =
            unsafe { core::slice::from_raw_parts_mut(source.base_addr as *mut f64, 4) };
        source_values.copy_from_slice(&[1.0, 2.0, 3.0, 4.0]);

        afs_copy_array_data_no_realloc(&mut dest, &source, ptr::null_mut());
        assert_eq!(dest.base_addr, dest_base);
        assert!(dest.is_allocated());
        let dest_values = unsafe { core::slice::from_raw_parts(dest.base_addr as *const f64, 4) };
        assert_eq!(dest_values, &[1.0, 2.0, 3.0, 4.0]);

        let mut borrowed_values = [0.0_f64; 4];
        let mut borrowed = ArrayDescriptor::zeroed();
        borrowed.base_addr = borrowed_values.as_mut_ptr() as *mut u8;
        borrowed.elem_size = 8;
        borrowed.rank = 2;
        borrowed.flags = DESC_CONTIGUOUS;
        borrowed.dims[0] = DimDescriptor {
            lower_bound: 1,
            upper_bound: 2,
            stride: 1,
        };
        borrowed.dims[1] = DimDescriptor {
            lower_bound: 1,
            upper_bound: 2,
            stride: 2,
        };
        afs_copy_array_data_no_realloc(&mut borrowed, &source, ptr::null_mut());
        assert_eq!(borrowed_values, [1.0, 2.0, 3.0, 4.0]);
        assert!(!borrowed.is_allocated());

        let mismatch_dims = [DimDescriptor {
            lower_bound: 1,
            upper_bound: 3,
            stride: 1,
        }];
        let mut mismatch = ArrayDescriptor::zeroed();
        afs_allocate_array(&mut mismatch, 8, 1, mismatch_dims.as_ptr(), ptr::null_mut());
        let mut stat = 0;
        afs_copy_array_data_no_realloc(&mut dest, &mismatch, &mut stat);
        assert_eq!(stat, 4);
        assert_eq!(dest.base_addr, dest_base);
        assert!(dest.is_allocated());

        afs_deallocate_array(&mut dest, ptr::null_mut());
        afs_deallocate_array(&mut source, ptr::null_mut());
        afs_deallocate_array(&mut mismatch, ptr::null_mut());
    }

    #[test]
    fn array_assignment_conformance_compares_rank_and_each_extent() {
        let mut dest = ArrayDescriptor::zeroed();
        dest.rank = 2;
        dest.dims[0] = DimDescriptor {
            lower_bound: 1,
            upper_bound: 2,
            stride: 1,
        };
        dest.dims[1] = DimDescriptor {
            lower_bound: 1,
            upper_bound: 3,
            stride: 2,
        };

        let mut matching = ArrayDescriptor::zeroed();
        matching.rank = 2;
        matching.dims[0] = DimDescriptor {
            lower_bound: -3,
            upper_bound: -2,
            stride: 1,
        };
        matching.dims[1] = DimDescriptor {
            lower_bound: 7,
            upper_bound: 9,
            stride: 2,
        };
        assert!(array_assignment_shapes_conform(&dest, &matching));

        let mut transposed_shape = matching;
        transposed_shape.dims[0].upper_bound = -1;
        transposed_shape.dims[1].upper_bound = 8;
        assert_eq!(transposed_shape.total_elements(), dest.total_elements());
        assert!(!array_assignment_shapes_conform(&dest, &transposed_shape));

        let mut different_rank = ArrayDescriptor::zeroed();
        different_rank.rank = 1;
        different_rank.dims[0] = DimDescriptor {
            lower_bound: 1,
            upper_bound: 6,
            stride: 1,
        };
        assert_eq!(different_rank.total_elements(), dest.total_elements());
        assert!(!array_assignment_shapes_conform(&dest, &different_rank));
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
    fn assign_allocatable_self_section_snapshots_before_reallocate() {
        let mut desc = ArrayDescriptor::zeroed();
        let mut section = ArrayDescriptor::zeroed();

        afs_allocate_1d(&mut desc, 4, 4);
        unsafe {
            let data = desc.base_addr as *mut i32;
            *data.add(0) = 10;
            *data.add(1) = 20;
            *data.add(2) = 30;
            *data.add(3) = 40;
        }

        let spec = SectionSpec {
            start: 2,
            end: 4,
            stride: 1,
        };
        afs_create_section(&desc, &mut section, &spec, 1);

        afs_assign_allocatable(&mut desc, &section);
        assert!(desc.is_allocated());
        assert_eq!(desc.total_elements(), 3);
        unsafe {
            let data = desc.base_addr as *const i32;
            assert_eq!(*data.add(0), 20);
            assert_eq!(*data.add(1), 30);
            assert_eq!(*data.add(2), 40);
        }

        afs_deallocate_array(&mut desc, ptr::null_mut());
    }

    #[test]
    fn assign_allocatable_negative_self_section_walks_stride() {
        let mut desc = ArrayDescriptor::zeroed();
        let mut section = ArrayDescriptor::zeroed();

        afs_allocate_1d(&mut desc, 4, 4);
        unsafe {
            let data = desc.base_addr as *mut i32;
            *data.add(0) = 10;
            *data.add(1) = 20;
            *data.add(2) = 30;
            *data.add(3) = 40;
        }

        let spec = SectionSpec {
            start: 4,
            end: 1,
            stride: -3,
        };
        afs_create_section(&desc, &mut section, &spec, 1);

        afs_assign_allocatable(&mut desc, &section);
        assert!(desc.is_allocated());
        assert_eq!(desc.total_elements(), 2);
        unsafe {
            let data = desc.base_addr as *const i32;
            assert_eq!(*data.add(0), 40);
            assert_eq!(*data.add(1), 10);
        }

        afs_deallocate_array(&mut desc, ptr::null_mut());
    }

    #[test]
    fn descriptor_overlap_covers_scalar_and_strided_views() {
        let mut left_data = [10_i32, 20, 30, 40];
        let mut right_data = [50_i32, 60];
        let left = i32_descriptor(&mut left_data, &[4]);
        let right = i32_descriptor(&mut right_data, &[2]);
        assert_eq!(afs_descriptors_overlap(&left, &right), 0);

        let mut scalar_alias = ArrayDescriptor::zeroed();
        scalar_alias.base_addr = left.base_addr;
        scalar_alias.elem_size = left.elem_size;
        assert_eq!(afs_descriptors_overlap(&left, &scalar_alias), 1);

        let positive_spec = SectionSpec {
            start: 2,
            end: 4,
            stride: 2,
        };
        let mut positive = ArrayDescriptor::zeroed();
        afs_create_section(&left, &mut positive, &positive_spec, 1);
        assert_eq!(afs_descriptors_overlap(&left, &positive), 1);

        let negative_spec = SectionSpec {
            start: 4,
            end: 1,
            stride: -1,
        };
        let mut negative = ArrayDescriptor::zeroed();
        afs_create_section(&left, &mut negative, &negative_spec, 1);
        assert_eq!(afs_descriptors_overlap(&left, &negative), 1);

        let empty_spec = SectionSpec {
            start: 3,
            end: 2,
            stride: 1,
        };
        let mut empty = ArrayDescriptor::zeroed();
        afs_create_section(&left, &mut empty, &empty_spec, 1);
        assert_eq!(afs_descriptors_overlap(&left, &empty), 0);
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
    fn logical_reductions_use_identities_for_zero_size_arrays() {
        let mut desc = ArrayDescriptor::zeroed();
        let dims = [
            DimDescriptor {
                lower_bound: 1,
                upper_bound: 2,
                stride: 1,
            },
            DimDescriptor {
                lower_bound: 1,
                upper_bound: 0,
                stride: 2,
            },
        ];
        afs_allocate_array(&mut desc, 1, 2, dims.as_ptr(), ptr::null_mut());

        assert!(desc.is_allocated());
        assert_eq!(desc.total_elements(), 0);
        assert!(desc.base_addr.is_null());
        assert_eq!(afs_array_all_logical(&desc), 1);
        assert_eq!(afs_array_any_logical(&desc), 0);
        assert_eq!(afs_array_count_logical(&desc), 0);

        afs_deallocate_array(&mut desc, ptr::null_mut());
    }

    #[test]
    fn array_size_unallocated_descriptor_returns_zero() {
        let mut desc = ArrayDescriptor::zeroed();
        desc.elem_size = 4;
        desc.rank = 1;
        desc.dims[0] = DimDescriptor {
            lower_bound: 1,
            upper_bound: 1,
            stride: 1,
        };

        assert!(!desc.is_allocated());
        assert_eq!(afs_array_size(&desc), 0);
        assert_eq!(afs_array_size_dim(&desc, 1), 0);
    }

    #[test]
    fn array_size_nonowning_data_descriptor_uses_shape() {
        let mut data = [1i32, 2, 3, 4];
        let mut desc = ArrayDescriptor::zeroed();
        desc.base_addr = data.as_mut_ptr() as *mut u8;
        desc.elem_size = 4;
        desc.rank = 1;
        desc.flags = DESC_CONTIGUOUS;
        desc.dims[0] = DimDescriptor {
            lower_bound: 1,
            upper_bound: 4,
            stride: 1,
        };

        assert!(!desc.is_allocated());
        assert_eq!(afs_array_size(&desc), 4);
        assert_eq!(afs_array_size_dim(&desc, 1), 4);
    }

    #[test]
    fn pack_zero_size_allocated_arrays_returns_zero_size_result() {
        let mut src = ArrayDescriptor::zeroed();
        let mut mask = ArrayDescriptor::zeroed();
        let mut result = ArrayDescriptor::zeroed();

        afs_allocate_1d(&mut src, 1, 0);
        afs_allocate_1d(&mut mask, 1, 0);
        afs_array_pack(&src, &mask, ptr::null(), &mut result);

        assert!(result.is_allocated());
        assert_eq!(result.rank, 1);
        assert_eq!(result.total_elements(), 0);
        assert!(result.base_addr.is_null());

        afs_deallocate_array(&mut src, ptr::null_mut());
        afs_deallocate_array(&mut mask, ptr::null_mut());
        afs_deallocate_array(&mut result, ptr::null_mut());
    }

    #[test]
    fn pack_strided_row_section_respects_descriptor_strides() {
        let mut src = ArrayDescriptor::zeroed();
        let mut mask = ArrayDescriptor::zeroed();
        let mut src_row = ArrayDescriptor::zeroed();
        let mut mask_row = ArrayDescriptor::zeroed();
        let mut result = ArrayDescriptor::zeroed();
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
        afs_allocate_array(&mut src, 1, 2, dims.as_ptr(), ptr::null_mut());
        afs_allocate_array(&mut mask, 1, 2, dims.as_ptr(), ptr::null_mut());

        let values = [10_i8, 2, -3, -4, 6, -6, 7, -8, 9, 0, 1, 20];
        unsafe {
            core::ptr::copy_nonoverlapping(
                values.as_ptr() as *const u8,
                src.base_addr,
                values.len(),
            );
            let mask_buf = mask.base_addr;
            for (i, value) in values.iter().enumerate() {
                *mask_buf.add(i) = u8::from(*value > 0);
            }
        }

        let specs = [
            SectionSpec {
                start: 1,
                end: 1,
                stride: 0,
            },
            SectionSpec {
                start: 1,
                end: 4,
                stride: 1,
            },
        ];
        afs_create_section(&src, &mut src_row, specs.as_ptr(), 2);
        afs_create_section(&mask, &mut mask_row, specs.as_ptr(), 2);
        afs_array_pack(&src_row, &mask_row, ptr::null(), &mut result);

        assert!(result.is_allocated());
        assert_eq!(result.rank, 1);
        assert_eq!(result.total_elements(), 2);
        unsafe {
            let data = result.base_addr as *const i8;
            assert_eq!(*data.add(0), 10);
            assert_eq!(*data.add(1), 7);
        }

        afs_deallocate_array(&mut src, ptr::null_mut());
        afs_deallocate_array(&mut mask, ptr::null_mut());
        afs_deallocate_array(&mut result, ptr::null_mut());
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

    #[test]
    fn iolength_accumulation_is_checked() {
        assert_eq!(checked_iolength_accumulate(3, 4, 8), Some(35));
        assert_eq!(checked_iolength_accumulate(7, 0, i64::MAX), Some(7));
        assert_eq!(checked_iolength_accumulate(-1, 1, 1), None);
        assert_eq!(checked_iolength_accumulate(0, -1, 1), None);
        assert_eq!(checked_iolength_accumulate(0, 1, -1), None);
        assert_eq!(checked_iolength_accumulate(i64::MAX, 1, 1), None);
        assert_eq!(checked_iolength_accumulate(0, i64::MAX, 2), None);
    }

    #[test]
    fn iolength_array_accumulation_checks_rank_and_extent_products() {
        let mut desc = ArrayDescriptor::zeroed();
        desc.rank = 2;
        desc.dims[0] = DimDescriptor {
            lower_bound: -2,
            upper_bound: 2,
            stride: 1,
        };
        desc.dims[1] = DimDescriptor {
            lower_bound: 4,
            upper_bound: 6,
            stride: 5,
        };
        assert_eq!(checked_iolength_array_accumulate(11, &desc, 8), Some(131));

        desc.dims[1].upper_bound = 3;
        assert_eq!(
            checked_iolength_array_accumulate(11, &desc, i64::MAX),
            Some(11)
        );

        desc.rank = (MAX_RANK + 1) as i32;
        assert_eq!(checked_iolength_array_accumulate(0, &desc, 1), None);

        desc.rank = 2;
        desc.dims[0] = DimDescriptor {
            lower_bound: 0,
            upper_bound: i64::MAX,
            stride: 1,
        };
        assert_eq!(checked_iolength_array_accumulate(0, &desc, 1), None);
    }
}

// ---- Array query intrinsics ----

fn checked_iolength_accumulate(total: i64, count: i64, elem_size: i64) -> Option<i64> {
    if total < 0 || count < 0 || elem_size < 0 {
        return None;
    }
    count
        .checked_mul(elem_size)
        .and_then(|bytes| total.checked_add(bytes))
}

fn checked_iolength_array_accumulate(
    total: i64,
    desc: &ArrayDescriptor,
    elem_size: i64,
) -> Option<i64> {
    if desc.rank < 0 || desc.rank as usize > MAX_RANK {
        return None;
    }
    let mut count = 1i64;
    for dim in desc.dims.iter().copied().take(desc.rank as usize) {
        let extent = checked_dim_extent(dim)?;
        count = count.checked_mul(extent)?;
    }
    checked_iolength_accumulate(total, count, elem_size)
}

fn report_iolength_overflow() -> ! {
    eprintln!("INQUIRE(IOLENGTH=): result size overflows INTEGER(8)");
    std::process::exit(1);
}

/// Add `count * elem_size` file-storage units to an IOLENGTH accumulator.
///
/// The compiler uses this entry point even for statically shaped objects so
/// target-independent lowering never relies on wrapping machine arithmetic.
#[no_mangle]
pub extern "C" fn afs_iolength_add(total: i64, count: i64, elem_size: i64) -> i64 {
    checked_iolength_accumulate(total, count, elem_size)
        .unwrap_or_else(|| report_iolength_overflow())
}

/// Add the transfer size of every logical element in an array descriptor.
///
/// `elem_size` is the unformatted transfer width, which can intentionally
/// differ from the descriptor's storage stride (default LOGICAL is the
/// important case). Bounds are multiplied with checked arithmetic.
#[no_mangle]
pub extern "C" fn afs_iolength_add_array(
    total: i64,
    desc: *const ArrayDescriptor,
    elem_size: i64,
) -> i64 {
    if desc.is_null() {
        eprintln!("INQUIRE(IOLENGTH=): missing array descriptor");
        std::process::exit(1);
    }
    let desc = unsafe { &*desc };
    checked_iolength_array_accumulate(total, desc, elem_size)
        .unwrap_or_else(|| report_iolength_overflow())
}

/// SIZE(array) — total number of elements.
#[no_mangle]
pub extern "C" fn afs_array_size(desc: *const ArrayDescriptor) -> i64 {
    if desc.is_null() {
        return 0;
    }
    let d = unsafe { &*desc };
    if !d.is_allocated() && d.base_addr.is_null() {
        return 0;
    }
    d.total_elements()
}

/// SIZE(array, dim) — number of elements along dimension `dim` (1-based).
#[no_mangle]
pub extern "C" fn afs_array_size_dim(desc: *const ArrayDescriptor, dim: i32) -> i64 {
    if desc.is_null() || dim < 1 {
        return 0;
    }
    let d = unsafe { &*desc };
    if !d.is_allocated() && d.base_addr.is_null() {
        return 0;
    }
    let idx = (dim - 1) as usize;
    if idx < d.rank as usize {
        d.dims[idx].extent()
    } else {
        0
    }
}

/// SHAPE(array) → fresh rank-1 default-integer (i32) array of length
/// `rank`, holding each dimension's extent. Allocates the destination
/// via `afs_allocate_array`. F2018 §16.9.207.
#[no_mangle]
pub extern "C" fn afs_array_shape_int4(dst: *mut ArrayDescriptor, src: *const ArrayDescriptor) {
    if dst.is_null() || src.is_null() {
        return;
    }
    let s = unsafe { &*src };
    let n = s.rank as i64;
    let dim = DimDescriptor {
        lower_bound: 1,
        upper_bound: n,
        stride: 1,
    };
    afs_allocate_array(dst, 4, 1, &dim as *const DimDescriptor, ptr::null_mut());
    let d = unsafe { &mut *dst };
    let base = d.base_addr as *mut i32;
    for i in 0..s.rank as usize {
        unsafe {
            base.add(i).write(s.dims[i].extent() as i32);
        }
    }
}

/// SHAPE(array, kind=int64) → rank-1 i64 array of extents.
#[no_mangle]
pub extern "C" fn afs_array_shape_int8(dst: *mut ArrayDescriptor, src: *const ArrayDescriptor) {
    if dst.is_null() || src.is_null() {
        return;
    }
    let s = unsafe { &*src };
    let n = s.rank as i64;
    let dim = DimDescriptor {
        lower_bound: 1,
        upper_bound: n,
        stride: 1,
    };
    afs_allocate_array(dst, 8, 1, &dim as *const DimDescriptor, ptr::null_mut());
    let d = unsafe { &mut *dst };
    let base = d.base_addr as *mut i64;
    for i in 0..s.rank as usize {
        unsafe {
            base.add(i).write(s.dims[i].extent());
        }
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
        let dimension = d.dims[idx];
        if dimension.extent() == 0 {
            1
        } else {
            dimension.lower_bound
        }
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
        let dimension = d.dims[idx];
        if dimension.extent() == 0 {
            0
        } else {
            dimension.upper_bound
        }
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

fn logical_desc_value(desc: &ArrayDescriptor, index: usize) -> bool {
    if desc.base_addr.is_null() || desc.rank < 1 {
        return false;
    }
    let rank = desc.rank as usize;
    let elem_size = desc.elem_size.max(1);
    let mut linear = index;
    let mut byte_offset = 0i64;
    for d in 0..rank {
        let extent = desc.dims[d].extent().max(1) as usize;
        let coord = (linear % extent) as i64;
        linear /= extent;
        byte_offset += coord * desc.dims[d].stride * elem_size;
    }
    let ptr = unsafe { desc.base_addr.offset(byte_offset as isize) };
    unsafe { *ptr != 0 }
}

/// ANY(array) for logical arrays — return 1 when any element is true.
#[no_mangle]
pub extern "C" fn afs_array_any_logical(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() {
        return 0;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 0;
    }
    let mut found = false;
    for_each_element_byte_offset(d, |byte_off| unsafe {
        if mask_byte_offset_is_true(d, byte_off) {
            found = true;
        }
    });
    if found {
        1
    } else {
        0
    }
}

/// ALL(array) for logical arrays — return 1 when every element is true.
#[no_mangle]
pub extern "C" fn afs_array_all_logical(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() {
        return 0;
    }
    let d = unsafe { &*desc };
    if !descriptor_has_payload_or_zero_size_array(d) {
        return 0;
    }
    let mut all_true = true;
    for_each_element_byte_offset(d, |byte_off| unsafe {
        if !mask_byte_offset_is_true(d, byte_off) {
            all_true = false;
        }
    });
    if all_true {
        1
    } else {
        0
    }
}

fn array_logical_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    is_all: bool,
) {
    if src.is_null() || dst.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    if !descriptor_has_payload_or_zero_size_array(s) || dim as usize > s.rank as usize {
        return;
    }
    if !ensure_reduction_dim_result(s, dim, dst, 1) {
        return;
    }
    let d = unsafe { &mut *dst };
    let dst_total = d.total_elements() as usize;
    let out = d.base_addr;
    if out.is_null() {
        return;
    }
    let init = if is_all { 1u8 } else { 0u8 };
    for i in 0..dst_total {
        unsafe {
            *out.add(i) = init;
        }
    }
    for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
        let truth = unsafe { mask_byte_offset_is_true(s, byte_off as isize) };
        unsafe {
            let slot = out.add(dst_flat);
            if is_all {
                *slot = u8::from(*slot != 0 && truth);
            } else {
                *slot = u8::from(*slot != 0 || truth);
            }
        }
    });
}

/// ANY(array, DIM=k) for logical arrays.
#[no_mangle]
pub extern "C" fn afs_array_any_logical_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    array_logical_dim(src, dim, dst, false);
}

/// ALL(array, DIM=k) for logical arrays.
#[no_mangle]
pub extern "C" fn afs_array_all_logical_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    array_logical_dim(src, dim, dst, true);
}

/// COUNT(array) for logical arrays — number of true elements.
#[no_mangle]
pub extern "C" fn afs_array_count_logical(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() {
        return 0;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 0;
    }
    let mut count = 0i32;
    for_each_element_byte_offset(d, |byte_off| unsafe {
        if mask_byte_offset_is_true(d, byte_off) {
            count += 1;
        }
    });
    count
}

/// COUNT(mask, DIM=k) — reduce along dimension k, allocate `dst` with
/// rank `mask.rank - 1` and extents = mask extents minus the reduction
/// dim, fill with per-slice counts of true elements (i32). Caller passes
/// a zeroed 392-byte descriptor; this helper populates it. Surfaced in
/// stdlib_stats var_mask_2_*: `n = count(mask, dim)` where n is rank-1
/// (and a real array — caller does the int→real conversion after).
/// Without this helper count(mask, dim) lowered to the rank-0 helper
/// and returned a single int, which the compiler then passed as the
/// source descriptor pointer to afs_assign_allocatable, crashing with
/// a misaligned-pointer dereference (address 0x3 = the count value).
// The column-major stride loops below intentionally use indexed access
// across `extents`, `idx`, `dst_running_stride`, and `s.dims` together
// with a separately-incrementing `dk` counter — clippy's
// `enumerate().take(rank)` rewrite doesn't apply cleanly without
// duplicating the index plumbing.
#[allow(clippy::needless_range_loop)]
#[no_mangle]
pub extern "C" fn afs_array_count_logical_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() {
        return;
    }
    let s = unsafe { &*src };
    let d = unsafe { &mut *dst };
    if !descriptor_has_payload_or_zero_size_array(s) {
        return;
    }
    if !d.is_allocated() {
        let new_rank = (s.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..s.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = s.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        // Result is integer(int32) per F2018 §16.9.46 default kind.
        afs_allocate_array(dst, 4, new_rank, dim_ptr, &mut stat);
        if stat != 0 || d.base_addr.is_null() {
            return;
        }
    }
    let dst_total = d.total_elements() as usize;
    let buf = d.base_addr as *mut i32;
    for i in 0..dst_total {
        unsafe {
            *buf.add(i) = 0;
        }
    }
    // Walk the mask in column-major order. logical_desc_value uses a
    // flat index which itself does column-major stride math, so we
    // mirror that here rather than use for_each_reduce_along_dim
    // (which gives byte offsets — not what logical_desc_value wants).
    let rank = s.rank as usize;
    if rank == 0 {
        return;
    }
    let reduce_dim_idx = dim as usize - 1;
    if reduce_dim_idx >= rank {
        return;
    }
    let mut extents: [i64; 15] = [0; 15];
    let mut dst_running_stride: [i64; 15] = [0; 15];
    let mut k = 0usize;
    let mut acc = 1i64;
    for i in 0..rank {
        extents[i] = s.dims[i].extent();
        if i == reduce_dim_idx {
            continue;
        }
        dst_running_stride[k] = acc;
        acc *= extents[i];
        k += 1;
    }
    let mut idx: [i64; 15] = [0; 15];
    let total = (0..rank).map(|i| extents[i]).product::<i64>();
    if total <= 0 {
        return;
    }
    for _ in 0..total {
        // Flat (column-major) index into the source mask.
        let mut src_flat: i64 = 0;
        let mut src_stride: i64 = 1;
        for d_i in 0..rank {
            src_flat += idx[d_i] * src_stride;
            src_stride *= extents[d_i];
        }
        let mut dst_flat: i64 = 0;
        let mut dk = 0usize;
        for d_i in 0..rank {
            if d_i != reduce_dim_idx {
                dst_flat += idx[d_i] * dst_running_stride[dk];
                dk += 1;
            }
        }
        if logical_desc_value(s, src_flat as usize) {
            unsafe {
                *buf.add(dst_flat as usize) += 1;
            }
        }
        for d_i in 0..rank {
            idx[d_i] += 1;
            if idx[d_i] < extents[d_i] {
                break;
            }
            idx[d_i] = 0;
        }
    }
}

/// NORM2(array) — Euclidean norm `sqrt(sum(x**2))` (real(8)).
/// F2018 §16.9.158. Respects strides for non-contiguous sections.
#[no_mangle]
pub extern "C" fn afs_array_norm2_real8(desc: *const ArrayDescriptor) -> f64 {
    if desc.is_null() {
        return 0.0;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 0.0;
    }
    let mut acc = 0.0_f64;
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset(d, |byte_off| {
        let v = unsafe { *(src.offset(byte_off) as *const f64) };
        acc += v * v;
    });
    acc.sqrt()
}

/// NORM2(array) — Euclidean norm `sqrt(sum(x**2))` (real(4)).
#[no_mangle]
pub extern "C" fn afs_array_norm2_real4(desc: *const ArrayDescriptor) -> f32 {
    if desc.is_null() {
        return 0.0;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 0.0;
    }
    let mut acc = 0.0_f64;
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset(d, |byte_off| {
        let v = unsafe { *(src.offset(byte_off) as *const f32) } as f64;
        acc += v * v;
    });
    acc.sqrt() as f32
}

#[no_mangle]
pub extern "C" fn afs_array_norm2_real8_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    if !descriptor_has_payload_or_zero_size_array(s) || dim as usize > s.rank as usize {
        return;
    }
    let d = unsafe { &mut *dst };
    if !d.is_allocated() {
        let new_rank = (s.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..s.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = s.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, 8, new_rank, dim_ptr, &mut stat);
        if stat != 0 || d.base_addr.is_null() {
            return;
        }
    }
    let dst_total = d.total_elements() as usize;
    let buf = d.base_addr as *mut f64;
    for i in 0..dst_total {
        unsafe {
            *buf.add(i) = 0.0;
        }
    }
    let src_ptr = s.base_addr as *const u8;
    for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
        let v = unsafe { *(src_ptr.add(byte_off) as *const f64) };
        unsafe {
            *buf.add(dst_flat) += v * v;
        }
    });
    for i in 0..dst_total {
        unsafe {
            *buf.add(i) = (*buf.add(i)).sqrt();
        }
    }
}

#[no_mangle]
pub extern "C" fn afs_array_norm2_real4_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    if !descriptor_has_payload_or_zero_size_array(s) || dim as usize > s.rank as usize {
        return;
    }
    let d = unsafe { &mut *dst };
    if !d.is_allocated() {
        let new_rank = (s.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..s.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = s.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, 4, new_rank, dim_ptr, &mut stat);
        if stat != 0 || d.base_addr.is_null() {
            return;
        }
    }
    let dst_total = d.total_elements() as usize;
    let mut acc = vec![0.0_f64; dst_total];
    let src_ptr = s.base_addr as *const u8;
    for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
        let v = unsafe { *(src_ptr.add(byte_off) as *const f32) } as f64;
        acc[dst_flat] += v * v;
    });
    let buf = d.base_addr as *mut f32;
    for (i, value) in acc.into_iter().enumerate() {
        unsafe {
            *buf.add(i) = value.sqrt() as f32;
        }
    }
}

/// SUM(array) — sum all elements (real version).
/// Dispatches on `elem_size` so real(4) and real(8) arrays both sum
/// correctly. Returns f64 (callers downcast for real(4) destinations).
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
    let mut sum: f64 = 0.0;
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset(d, |byte_off| unsafe {
        sum += read_real_as_f64(src, byte_off, d.elem_size);
    });
    sum
}

/// Walk every element of an n-dimensional array `src`, computing the
/// flat byte offset of each element relative to `src.base_addr` and
/// the corresponding flat dst index after collapsing dimension
/// `reduce_dim` (1-based) — the dst array has rank `src.rank - 1` with
/// extents copied from src skipping the reduction dim.
///
/// The closure `accum(byte_offset, dst_flat_idx)` is invoked once per
/// element. Caller supplies the accumulator/store logic for whatever
/// reduction is being computed (sum, product, maxval, minval, etc.).
fn for_each_reduce_along_dim<F: FnMut(usize, usize)>(
    src: &ArrayDescriptor,
    reduce_dim: i32,
    mut accum: F,
) {
    let rank = src.rank as usize;
    if rank == 0 {
        return;
    }
    let reduce_dim_idx = reduce_dim as usize - 1;
    if reduce_dim_idx >= rank {
        return;
    }
    let mut extents: [i64; 15] = [0; 15];
    let mut strides: [i64; 15] = [0; 15];
    // Layout of dst dims (rank - 1) — extents from src minus reduce_dim;
    // computed running stride for column-major dst layout.
    let mut dst_extents: [i64; 15] = [0; 15];
    let mut dst_running_stride: [i64; 15] = [0; 15];
    let mut k = 0usize;
    let mut acc = 1i64;
    for i in 0..rank {
        extents[i] = src.dims[i].extent();
        strides[i] = src.dims[i].stride.max(1);
        if i == reduce_dim_idx {
            continue;
        }
        dst_extents[k] = extents[i];
        dst_running_stride[k] = acc;
        acc *= extents[i];
        k += 1;
    }
    let mut idx: [i64; 15] = [0; 15];
    let total = (0..rank).map(|i| extents[i]).product::<i64>();
    if total <= 0 {
        return;
    }
    for _ in 0..total {
        let mut byte_off: i64 = 0;
        let mut dst_flat: i64 = 0;
        let mut dk = 0usize;
        for d in 0..rank {
            byte_off += idx[d] * strides[d] * src.elem_size;
            if d != reduce_dim_idx {
                dst_flat += idx[d] * dst_running_stride[dk];
                dk += 1;
            }
        }
        accum(byte_off as usize, dst_flat as usize);
        // Increment idx in column-major order (innermost = idx[0]).
        for d in 0..rank {
            idx[d] += 1;
            if idx[d] < extents[d] {
                break;
            }
            idx[d] = 0;
        }
    }
}

fn for_each_reduce_along_dim_optional_mask_with_index<
    F: FnMut(usize, Option<usize>, usize, i64),
>(
    src: &ArrayDescriptor,
    mask: Option<&ArrayDescriptor>,
    reduce_dim: i32,
    mut accum: F,
) {
    let rank = src.rank as usize;
    if rank == 0 {
        return;
    }
    let reduce_dim_idx = reduce_dim as usize - 1;
    if reduce_dim_idx >= rank {
        return;
    }
    let mut extents: [i64; 15] = [0; 15];
    let mut s_strides: [i64; 15] = [0; 15];
    let mut m_strides: [i64; 15] = [0; 15];
    let mut dst_running_stride: [i64; 15] = [0; 15];
    let mut k = 0usize;
    let mut acc = 1i64;
    for i in 0..rank {
        extents[i] = src.dims[i].extent();
        s_strides[i] = src.dims[i].stride.max(1);
        m_strides[i] = mask
            .filter(|m| (i as i32) < m.rank)
            .map_or(1, |m| m.dims[i].stride.max(1));
        if i == reduce_dim_idx {
            continue;
        }
        dst_running_stride[k] = acc;
        acc *= extents[i];
        k += 1;
    }
    let total = (0..rank).map(|i| extents[i]).product::<i64>();
    if total <= 0 {
        return;
    }
    let mask_elem = mask.map_or(1, |m| m.elem_size.max(1));
    let mut idx: [i64; 15] = [0; 15];
    for _ in 0..total {
        let mut s_byte_off: i64 = 0;
        let mut m_byte_off: i64 = 0;
        let mut dst_flat: i64 = 0;
        let mut dk = 0usize;
        for d in 0..rank {
            s_byte_off += idx[d] * s_strides[d] * src.elem_size;
            if mask.is_some() {
                m_byte_off += idx[d] * m_strides[d] * mask_elem;
            }
            if d != reduce_dim_idx {
                dst_flat += idx[d] * dst_running_stride[dk];
                dk += 1;
            }
        }
        accum(
            s_byte_off as usize,
            mask.map(|_| m_byte_off as usize),
            dst_flat as usize,
            idx[reduce_dim_idx] + 1,
        );
        for d in 0..rank {
            idx[d] += 1;
            if idx[d] < extents[d] {
                break;
            }
            idx[d] = 0;
        }
    }
}

fn ensure_reduction_dim_result(
    src: &ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    elem_size: i64,
) -> bool {
    if dst.is_null() || dim < 1 || dim as usize > src.rank as usize || elem_size <= 0 {
        return false;
    }
    let d = unsafe { &mut *dst };
    if !d.is_allocated() {
        let new_rank = (src.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..src.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = src.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, elem_size, new_rank, dim_ptr, &mut stat);
        if stat != 0 {
            return false;
        }
    }
    let d = unsafe { &*dst };
    d.is_allocated() && (d.total_elements() == 0 || !d.base_addr.is_null())
}

fn ensure_location_dim_result(src: &ArrayDescriptor, dim: i32, dst: *mut ArrayDescriptor) -> bool {
    if dst.is_null() || dim < 1 || dim as usize > src.rank as usize {
        return false;
    }
    let d = unsafe { &mut *dst };
    if !d.is_allocated() {
        let new_rank = (src.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..src.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = src.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, 4, new_rank, dim_ptr, &mut stat);
        if stat != 0 {
            return false;
        }
    }
    let d = unsafe { &*dst };
    d.is_allocated() && (d.total_elements() == 0 || !d.base_addr.is_null())
}

fn array_loc_real_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    is_max: bool,
) {
    array_loc_real_dim_keywords(src, dim, dst, ptr::null(), -1, 0, is_max);
}

fn location_mask_allows(
    mask: Option<&ArrayDescriptor>,
    mask_byte_off: Option<usize>,
    mask_scalar: i32,
) -> bool {
    if let (Some(m), Some(byte_off)) = (mask, mask_byte_off) {
        return unsafe { mask_byte_is_true(m, byte_off) };
    }
    mask_scalar < 0 || mask_scalar != 0
}

fn array_loc_real_dim_keywords(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
    mask_scalar: i32,
    back: i32,
    is_max: bool,
) {
    if src.is_null() || dst.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    if !descriptor_has_payload_or_zero_size_array(s) || dim as usize > s.rank as usize {
        return;
    }
    let mask_desc = if mask.is_null() {
        None
    } else {
        let m = unsafe { &*mask };
        if !descriptor_has_payload_or_zero_size_array(m) {
            return;
        }
        Some(m)
    };
    if !ensure_location_dim_result(s, dim, dst) {
        return;
    }
    let d = unsafe { &mut *dst };
    let dst_total = d.total_elements() as usize;
    let out = d.base_addr as *mut i32;
    if !out.is_null() {
        unsafe {
            fill_i32_impl(out, dst_total, 0);
        }
    }
    if dst_total == 0 || s.total_elements() == 0 || s.base_addr.is_null() || out.is_null() {
        return;
    }
    let src_ptr = s.base_addr as *const u8;
    macro_rules! loc_real_kind {
        ($t:ty) => {{
            let mut seen = vec![false; dst_total];
            let mut best: Vec<$t> = vec![0 as $t; dst_total];
            for_each_reduce_along_dim_optional_mask_with_index(
                s,
                mask_desc,
                dim,
                |byte_off, mask_byte_off, dst_flat, reduce_index| {
                    if !location_mask_allows(mask_desc, mask_byte_off, mask_scalar) {
                        return;
                    }
                    let v = unsafe { *(src_ptr.add(byte_off) as *const $t) };
                    if !seen[dst_flat]
                        || (if is_max {
                            v > best[dst_flat]
                        } else {
                            v < best[dst_flat]
                        })
                        || (back != 0 && v == best[dst_flat])
                    {
                        seen[dst_flat] = true;
                        best[dst_flat] = v;
                        unsafe {
                            *out.add(dst_flat) = reduce_index as i32;
                        }
                    }
                },
            );
        }};
    }
    if s.elem_size == 4 {
        loc_real_kind!(f32);
    } else {
        loc_real_kind!(f64);
    }
}

fn array_loc_int_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    is_max: bool,
) {
    array_loc_int_dim_keywords(src, dim, dst, ptr::null(), -1, 0, is_max);
}

fn array_loc_int_dim_keywords(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
    mask_scalar: i32,
    back: i32,
    is_max: bool,
) {
    if src.is_null() || dst.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    if !descriptor_has_payload_or_zero_size_array(s) || dim as usize > s.rank as usize {
        return;
    }
    let mask_desc = if mask.is_null() {
        None
    } else {
        let m = unsafe { &*mask };
        if !descriptor_has_payload_or_zero_size_array(m) {
            return;
        }
        Some(m)
    };
    if !ensure_location_dim_result(s, dim, dst) {
        return;
    }
    let d = unsafe { &mut *dst };
    let dst_total = d.total_elements() as usize;
    let out = d.base_addr as *mut i32;
    if !out.is_null() {
        unsafe {
            fill_i32_impl(out, dst_total, 0);
        }
    }
    if dst_total == 0 || s.total_elements() == 0 || s.base_addr.is_null() || out.is_null() {
        return;
    }
    let src_ptr = s.base_addr as *const u8;
    macro_rules! loc_int_kind {
        ($t:ty) => {{
            let mut seen = vec![false; dst_total];
            let mut best: Vec<$t> = vec![0 as $t; dst_total];
            for_each_reduce_along_dim_optional_mask_with_index(
                s,
                mask_desc,
                dim,
                |byte_off, mask_byte_off, dst_flat, reduce_index| {
                    if !location_mask_allows(mask_desc, mask_byte_off, mask_scalar) {
                        return;
                    }
                    let v = unsafe { *(src_ptr.add(byte_off) as *const $t) };
                    if !seen[dst_flat]
                        || (if is_max {
                            v > best[dst_flat]
                        } else {
                            v < best[dst_flat]
                        })
                        || (back != 0 && v == best[dst_flat])
                    {
                        seen[dst_flat] = true;
                        best[dst_flat] = v;
                        unsafe {
                            *out.add(dst_flat) = reduce_index as i32;
                        }
                    }
                },
            );
        }};
    }
    match s.elem_size {
        1 => loc_int_kind!(i8),
        2 => loc_int_kind!(i16),
        8 => loc_int_kind!(i64),
        _ => loc_int_kind!(i32),
    }
}

fn array_findloc_real_dim_keywords(
    src: *const ArrayDescriptor,
    value: f64,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
    mask_scalar: i32,
    back: i32,
) {
    if src.is_null() || dst.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    if !descriptor_has_payload_or_zero_size_array(s) || dim as usize > s.rank as usize {
        return;
    }
    let mask_desc = if mask.is_null() {
        None
    } else {
        let m = unsafe { &*mask };
        if !descriptor_has_payload_or_zero_size_array(m) {
            return;
        }
        Some(m)
    };
    if !ensure_location_dim_result(s, dim, dst) {
        return;
    }
    let d = unsafe { &mut *dst };
    let dst_total = d.total_elements() as usize;
    let out = d.base_addr as *mut i32;
    if !out.is_null() {
        unsafe {
            fill_i32_impl(out, dst_total, 0);
        }
    }
    if dst_total == 0 || s.total_elements() == 0 || s.base_addr.is_null() || out.is_null() {
        return;
    }
    let src_ptr = s.base_addr as *const u8;
    let mut seen = vec![false; dst_total];
    for_each_reduce_along_dim_optional_mask_with_index(
        s,
        mask_desc,
        dim,
        |byte_off, mask_byte_off, dst_flat, reduce_index| {
            if !location_mask_allows(mask_desc, mask_byte_off, mask_scalar) {
                return;
            }
            let v = unsafe { read_real_as_f64(src_ptr, byte_off as isize, s.elem_size) };
            if v == value && (!seen[dst_flat] || back != 0) {
                seen[dst_flat] = true;
                unsafe {
                    *out.add(dst_flat) = reduce_index as i32;
                }
            }
        },
    );
}

fn array_findloc_int_dim_keywords(
    src: *const ArrayDescriptor,
    value: i64,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
    mask_scalar: i32,
    back: i32,
) {
    if src.is_null() || dst.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    if !descriptor_has_payload_or_zero_size_array(s) || dim as usize > s.rank as usize {
        return;
    }
    let mask_desc = if mask.is_null() {
        None
    } else {
        let m = unsafe { &*mask };
        if !descriptor_has_payload_or_zero_size_array(m) {
            return;
        }
        Some(m)
    };
    if !ensure_location_dim_result(s, dim, dst) {
        return;
    }
    let d = unsafe { &mut *dst };
    let dst_total = d.total_elements() as usize;
    let out = d.base_addr as *mut i32;
    if !out.is_null() {
        unsafe {
            fill_i32_impl(out, dst_total, 0);
        }
    }
    if dst_total == 0 || s.total_elements() == 0 || s.base_addr.is_null() || out.is_null() {
        return;
    }
    let src_ptr = s.base_addr as *const u8;
    let mut seen = vec![false; dst_total];
    for_each_reduce_along_dim_optional_mask_with_index(
        s,
        mask_desc,
        dim,
        |byte_off, mask_byte_off, dst_flat, reduce_index| {
            if !location_mask_allows(mask_desc, mask_byte_off, mask_scalar) {
                return;
            }
            let v = unsafe { read_int_as_i64(src_ptr, byte_off as isize, s.elem_size) };
            if v == value && (!seen[dst_flat] || back != 0) {
                seen[dst_flat] = true;
                unsafe {
                    *out.add(dst_flat) = reduce_index as i32;
                }
            }
        },
    );
}

fn array_findloc_logical_dim_keywords(
    src: *const ArrayDescriptor,
    value: i32,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
    mask_scalar: i32,
    back: i32,
) {
    if src.is_null() || dst.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    if !descriptor_has_payload_or_zero_size_array(s) || dim as usize > s.rank as usize {
        return;
    }
    let mask_desc = if mask.is_null() {
        None
    } else {
        let m = unsafe { &*mask };
        if !descriptor_has_payload_or_zero_size_array(m) {
            return;
        }
        Some(m)
    };
    if !ensure_location_dim_result(s, dim, dst) {
        return;
    }
    let d = unsafe { &mut *dst };
    let dst_total = d.total_elements() as usize;
    let out = d.base_addr as *mut i32;
    if !out.is_null() {
        unsafe {
            fill_i32_impl(out, dst_total, 0);
        }
    }
    if dst_total == 0 || s.total_elements() == 0 || s.base_addr.is_null() || out.is_null() {
        return;
    }
    let want = value != 0;
    let mut seen = vec![false; dst_total];
    for_each_reduce_along_dim_optional_mask_with_index(
        s,
        mask_desc,
        dim,
        |byte_off, mask_byte_off, dst_flat, reduce_index| {
            if !location_mask_allows(mask_desc, mask_byte_off, mask_scalar) {
                return;
            }
            let v = unsafe { mask_byte_offset_is_true(s, byte_off as isize) };
            if v == want && (!seen[dst_flat] || back != 0) {
                seen[dst_flat] = true;
                unsafe {
                    *out.add(dst_flat) = reduce_index as i32;
                }
            }
        },
    );
}

/// SUM(array, DIM=k) — reduce along dimension k, allocate `dst` with
/// rank `src.rank - 1` and extents = src extents minus the reduction
/// dim, then write the per-slice sums into dst. Caller passes a
/// zeroed 392-byte descriptor; this helper populates rank/dims/flags
/// and malloc's the result buffer. Real version (real4 + real8
/// dispatching on `src.elem_size`).
#[no_mangle]
pub extern "C" fn afs_array_sum_real8_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() {
        return;
    }
    let s = unsafe { &*src };
    let d = unsafe { &mut *dst };
    if !descriptor_has_payload_or_zero_size_array(s) {
        return;
    }
    if !d.is_allocated() {
        let new_rank = (s.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..s.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = s.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, s.elem_size, new_rank, dim_ptr, &mut stat);
        if stat != 0 || d.base_addr.is_null() {
            return;
        }
    }
    let dst_total = d.total_elements() as usize;
    if s.elem_size == 4 {
        let buf = d.base_addr as *mut f32;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = 0.0;
            }
        }
        let src_ptr = s.base_addr as *const u8;
        for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
            let v = unsafe { *(src_ptr.add(byte_off) as *const f32) };
            unsafe {
                *buf.add(dst_flat) += v;
            }
        });
    } else {
        let buf = d.base_addr as *mut f64;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = 0.0;
            }
        }
        let src_ptr = s.base_addr as *const u8;
        for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
            let v = unsafe { *(src_ptr.add(byte_off) as *const f64) };
            unsafe {
                *buf.add(dst_flat) += v;
            }
        });
    }
}

/// SUM(array, DIM=k) — integer version, dispatching on
/// `src.elem_size` (1/2/4/8). Result element width matches
/// `src.elem_size`. Auto-allocates `dst` if not already allocated.
#[no_mangle]
pub extern "C" fn afs_array_sum_int_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() {
        return;
    }
    let s = unsafe { &*src };
    let d = unsafe { &mut *dst };
    if !descriptor_has_payload_or_zero_size_array(s) {
        return;
    }
    if !d.is_allocated() {
        let new_rank = (s.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..s.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = s.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, s.elem_size, new_rank, dim_ptr, &mut stat);
        if stat != 0 || d.base_addr.is_null() {
            return;
        }
    }
    let dst_total = d.total_elements() as usize;
    let src_ptr = s.base_addr as *const u8;
    match s.elem_size {
        1 => {
            let buf = d.base_addr as *mut i8;
            for i in 0..dst_total {
                unsafe {
                    *buf.add(i) = 0;
                }
            }
            for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
                let v = unsafe { *(src_ptr.add(byte_off) as *const i8) };
                unsafe {
                    *buf.add(dst_flat) = (*buf.add(dst_flat)).wrapping_add(v);
                }
            });
        }
        2 => {
            let buf = d.base_addr as *mut i16;
            for i in 0..dst_total {
                unsafe {
                    *buf.add(i) = 0;
                }
            }
            for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
                let v = unsafe { *(src_ptr.add(byte_off) as *const i16) };
                unsafe {
                    *buf.add(dst_flat) = (*buf.add(dst_flat)).wrapping_add(v);
                }
            });
        }
        4 => {
            let buf = d.base_addr as *mut i32;
            for i in 0..dst_total {
                unsafe {
                    *buf.add(i) = 0;
                }
            }
            for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
                let v = unsafe { *(src_ptr.add(byte_off) as *const i32) };
                unsafe {
                    *buf.add(dst_flat) = (*buf.add(dst_flat)).wrapping_add(v);
                }
            });
        }
        _ => {
            let buf = d.base_addr as *mut i64;
            for i in 0..dst_total {
                unsafe {
                    *buf.add(i) = 0;
                }
            }
            for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
                let v = unsafe { *(src_ptr.add(byte_off) as *const i64) };
                unsafe {
                    *buf.add(dst_flat) = (*buf.add(dst_flat)).wrapping_add(v);
                }
            });
        }
    }
}

/// MAXVAL(array, DIM=k) - real version. Result element width matches
/// the source descriptor's element width.
#[no_mangle]
pub extern "C" fn afs_array_maxval_real8_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    if !descriptor_has_payload_or_zero_size_array(s) || dim as usize > s.rank as usize {
        return;
    }
    let d = unsafe { &mut *dst };
    if !d.is_allocated() {
        let new_rank = (s.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..s.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = s.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, s.elem_size, new_rank, dim_ptr, &mut stat);
        if stat != 0 || d.base_addr.is_null() {
            return;
        }
    }
    let dst_total = d.total_elements() as usize;
    let src_ptr = s.base_addr as *const u8;
    if s.elem_size == 4 {
        let buf = d.base_addr as *mut f32;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = -f32::MAX;
            }
        }
        for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
            let v = unsafe { *(src_ptr.add(byte_off) as *const f32) };
            unsafe {
                let slot = buf.add(dst_flat);
                if v > *slot {
                    *slot = v;
                }
            }
        });
    } else {
        let buf = d.base_addr as *mut f64;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = -f64::MAX;
            }
        }
        for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
            let v = unsafe { *(src_ptr.add(byte_off) as *const f64) };
            unsafe {
                let slot = buf.add(dst_flat);
                if v > *slot {
                    *slot = v;
                }
            }
        });
    }
}

/// MINVAL(array, DIM=k) - real version. Result element width matches
/// the source descriptor's element width.
#[no_mangle]
pub extern "C" fn afs_array_minval_real8_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    if !descriptor_has_payload_or_zero_size_array(s) || dim as usize > s.rank as usize {
        return;
    }
    let d = unsafe { &mut *dst };
    if !d.is_allocated() {
        let new_rank = (s.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..s.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = s.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, s.elem_size, new_rank, dim_ptr, &mut stat);
        if stat != 0 || d.base_addr.is_null() {
            return;
        }
    }
    let dst_total = d.total_elements() as usize;
    let src_ptr = s.base_addr as *const u8;
    if s.elem_size == 4 {
        let buf = d.base_addr as *mut f32;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = f32::MAX;
            }
        }
        for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
            let v = unsafe { *(src_ptr.add(byte_off) as *const f32) };
            unsafe {
                let slot = buf.add(dst_flat);
                if v < *slot {
                    *slot = v;
                }
            }
        });
    } else {
        let buf = d.base_addr as *mut f64;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = f64::MAX;
            }
        }
        for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
            let v = unsafe { *(src_ptr.add(byte_off) as *const f64) };
            unsafe {
                let slot = buf.add(dst_flat);
                if v < *slot {
                    *slot = v;
                }
            }
        });
    }
}

/// MAXVAL(array, DIM=k) - integer version.
#[no_mangle]
pub extern "C" fn afs_array_maxval_int_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    if !descriptor_has_payload_or_zero_size_array(s) || dim as usize > s.rank as usize {
        return;
    }
    let d = unsafe { &mut *dst };
    if !d.is_allocated() {
        let new_rank = (s.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..s.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = s.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, s.elem_size, new_rank, dim_ptr, &mut stat);
        if stat != 0 || d.base_addr.is_null() {
            return;
        }
    }
    let dst_total = d.total_elements() as usize;
    let src_ptr = s.base_addr as *const u8;
    macro_rules! max_dim_kind {
        ($t:ty, $identity:expr) => {{
            let buf = d.base_addr as *mut $t;
            for i in 0..dst_total {
                unsafe {
                    *buf.add(i) = $identity;
                }
            }
            for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
                let v = unsafe { *(src_ptr.add(byte_off) as *const $t) };
                unsafe {
                    let slot = buf.add(dst_flat);
                    if v > *slot {
                        *slot = v;
                    }
                }
            });
        }};
    }
    match s.elem_size {
        1 => max_dim_kind!(i8, i8::MIN),
        2 => max_dim_kind!(i16, i16::MIN),
        8 => max_dim_kind!(i64, i64::MIN),
        _ => max_dim_kind!(i32, i32::MIN),
    }
}

/// MINVAL(array, DIM=k) - integer version.
#[no_mangle]
pub extern "C" fn afs_array_minval_int_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    if !descriptor_has_payload_or_zero_size_array(s) || dim as usize > s.rank as usize {
        return;
    }
    let d = unsafe { &mut *dst };
    if !d.is_allocated() {
        let new_rank = (s.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..s.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = s.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, s.elem_size, new_rank, dim_ptr, &mut stat);
        if stat != 0 || d.base_addr.is_null() {
            return;
        }
    }
    let dst_total = d.total_elements() as usize;
    let src_ptr = s.base_addr as *const u8;
    macro_rules! min_dim_kind {
        ($t:ty, $identity:expr) => {{
            let buf = d.base_addr as *mut $t;
            for i in 0..dst_total {
                unsafe {
                    *buf.add(i) = $identity;
                }
            }
            for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
                let v = unsafe { *(src_ptr.add(byte_off) as *const $t) };
                unsafe {
                    let slot = buf.add(dst_flat);
                    if v < *slot {
                        *slot = v;
                    }
                }
            });
        }};
    }
    match s.elem_size {
        1 => min_dim_kind!(i8, i8::MAX),
        2 => min_dim_kind!(i16, i16::MAX),
        8 => min_dim_kind!(i64, i64::MAX),
        _ => min_dim_kind!(i32, i32::MAX),
    }
}

/// SUM(array, DIM=k) for complex(4). Auto-allocates `dst` to rank N-1
/// and writes interleaved real/imag f32 lanes.
#[no_mangle]
pub extern "C" fn afs_array_sum_complex4_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() {
        return;
    }
    let s = unsafe { &*src };
    let d = unsafe { &mut *dst };
    if !descriptor_has_payload_or_zero_size_array(s) {
        return;
    }
    if !d.is_allocated() {
        let new_rank = (s.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..s.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = s.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, s.elem_size, new_rank, dim_ptr, &mut stat);
        if stat != 0 || d.base_addr.is_null() {
            return;
        }
    }
    let dst_total = d.total_elements() as usize;
    let buf = d.base_addr as *mut f32;
    for i in 0..(dst_total * 2) {
        unsafe {
            *buf.add(i) = 0.0;
        }
    }
    let src_ptr = s.base_addr as *const u8;
    for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
        let p = unsafe { src_ptr.add(byte_off) as *const f32 };
        unsafe {
            *buf.add(dst_flat * 2) += *p.add(0);
            *buf.add(dst_flat * 2 + 1) += *p.add(1);
        }
    });
}

/// SUM(array, DIM=k) for complex(8). Auto-allocates `dst` to rank N-1
/// and writes interleaved real/imag f64 lanes.
#[no_mangle]
pub extern "C" fn afs_array_sum_complex8_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() {
        return;
    }
    let s = unsafe { &*src };
    let d = unsafe { &mut *dst };
    if !descriptor_has_payload_or_zero_size_array(s) {
        return;
    }
    if !d.is_allocated() {
        let new_rank = (s.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..s.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = s.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, s.elem_size, new_rank, dim_ptr, &mut stat);
        if stat != 0 || d.base_addr.is_null() {
            return;
        }
    }
    let dst_total = d.total_elements() as usize;
    let buf = d.base_addr as *mut f64;
    for i in 0..(dst_total * 2) {
        unsafe {
            *buf.add(i) = 0.0;
        }
    }
    let src_ptr = s.base_addr as *const u8;
    for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
        let p = unsafe { src_ptr.add(byte_off) as *const f64 };
        unsafe {
            *buf.add(dst_flat * 2) += *p.add(0);
            *buf.add(dst_flat * 2 + 1) += *p.add(1);
        }
    });
}

/// Walk every source element along all dims (column-major), invoking
/// `accum(src_byte_off, mask_byte_off, dst_flat)` so the caller can
/// honor both the source's and the mask's per-dim strides without
/// reimplementing the index machinery. `dst_flat` indexes the
/// rank-(N-1) output that excludes `reduce_dim`.
fn for_each_reduce_along_dim_with_mask<F: FnMut(usize, usize, usize)>(
    src: &ArrayDescriptor,
    mask: &ArrayDescriptor,
    reduce_dim: i32,
    mut accum: F,
) {
    let rank = src.rank as usize;
    if rank == 0 {
        return;
    }
    let reduce_dim_idx = reduce_dim as usize - 1;
    if reduce_dim_idx >= rank {
        return;
    }
    let mut extents: [i64; 15] = [0; 15];
    let mut s_strides: [i64; 15] = [0; 15];
    let mut m_strides: [i64; 15] = [0; 15];
    let mut dst_running_stride: [i64; 15] = [0; 15];
    let mut k = 0usize;
    let mut acc = 1i64;
    for i in 0..rank {
        extents[i] = src.dims[i].extent();
        s_strides[i] = src.dims[i].stride.max(1);
        m_strides[i] = if (i as i32) < mask.rank {
            mask.dims[i].stride.max(1)
        } else {
            1
        };
        if i == reduce_dim_idx {
            continue;
        }
        dst_running_stride[k] = acc;
        acc *= extents[i];
        k += 1;
    }
    let mut idx: [i64; 15] = [0; 15];
    let total = (0..rank).map(|i| extents[i]).product::<i64>();
    if total <= 0 {
        return;
    }
    let m_elem = mask.elem_size.max(1);
    for _ in 0..total {
        let mut s_byte_off: i64 = 0;
        let mut m_byte_off: i64 = 0;
        let mut dst_flat: i64 = 0;
        let mut dk = 0usize;
        for d in 0..rank {
            s_byte_off += idx[d] * s_strides[d] * src.elem_size;
            m_byte_off += idx[d] * m_strides[d] * m_elem;
            if d != reduce_dim_idx {
                dst_flat += idx[d] * dst_running_stride[dk];
                dk += 1;
            }
        }
        accum(s_byte_off as usize, m_byte_off as usize, dst_flat as usize);
        for d in 0..rank {
            idx[d] += 1;
            if idx[d] < extents[d] {
                break;
            }
            idx[d] = 0;
        }
    }
}

unsafe fn mask_byte_is_true(mask: &ArrayDescriptor, byte_off: usize) -> bool {
    let p = mask.base_addr.add(byte_off);
    match mask.elem_size {
        1 => *p != 0,
        2 => *(p as *const u16) != 0,
        4 => *(p as *const u32) != 0,
        8 => *(p as *const u64) != 0,
        _ => *p != 0,
    }
}

fn descriptor_linear_byte_offset(desc: &ArrayDescriptor, mut linear: usize) -> usize {
    let rank = desc.rank.max(0) as usize;
    if rank == 0 {
        return 0;
    }
    let elem_size = desc.elem_size.max(1);
    let mut byte_off = 0i64;
    for d in 0..rank {
        let extent = desc.dims[d].extent().max(1) as usize;
        let idx = (linear % extent) as i64;
        linear /= extent;
        byte_off += idx * desc.dims[d].stride.max(1) * elem_size;
    }
    byte_off as usize
}

/// SUM(array, DIM=k, MASK=mask) — real version. Auto-allocates `dst`
/// to rank-(N-1) on first call. Walks both array and mask using each
/// descriptor's own per-dim strides; treats any non-zero mask byte as
/// `.true.` (matches `mask_at`).
#[no_mangle]
pub extern "C" fn afs_array_sum_real8_dim_mask(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || mask.is_null() {
        return;
    }
    let s = unsafe { &*src };
    let d = unsafe { &mut *dst };
    let m = unsafe { &*mask };
    if !descriptor_has_payload_or_zero_size_array(s)
        || !descriptor_has_payload_or_zero_size_array(m)
    {
        return;
    }
    if !d.is_allocated() {
        let new_rank = (s.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..s.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = s.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, s.elem_size, new_rank, dim_ptr, &mut stat);
        if stat != 0 || d.base_addr.is_null() {
            return;
        }
    }
    let dst_total = d.total_elements() as usize;
    let src_ptr = s.base_addr as *const u8;
    if s.elem_size == 4 {
        let buf = d.base_addr as *mut f32;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = 0.0;
            }
        }
        for_each_reduce_along_dim_with_mask(s, m, dim, |sb, mb, df| {
            if unsafe { mask_byte_is_true(m, mb) } {
                let v = unsafe { *(src_ptr.add(sb) as *const f32) };
                unsafe {
                    *buf.add(df) += v;
                }
            }
        });
    } else {
        let buf = d.base_addr as *mut f64;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = 0.0;
            }
        }
        for_each_reduce_along_dim_with_mask(s, m, dim, |sb, mb, df| {
            if unsafe { mask_byte_is_true(m, mb) } {
                let v = unsafe { *(src_ptr.add(sb) as *const f64) };
                unsafe {
                    *buf.add(df) += v;
                }
            }
        });
    }
}

/// SUM(array, DIM=k, MASK=mask) — integer version. Dispatches on
/// `src.elem_size` (1/2/4/8); auto-allocates `dst` and walks both
/// descriptors with their own per-dim strides.
#[no_mangle]
pub extern "C" fn afs_array_sum_int_dim_mask(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || mask.is_null() {
        return;
    }
    let s = unsafe { &*src };
    let d = unsafe { &mut *dst };
    let m = unsafe { &*mask };
    if !descriptor_has_payload_or_zero_size_array(s)
        || !descriptor_has_payload_or_zero_size_array(m)
    {
        return;
    }
    if !d.is_allocated() {
        let new_rank = (s.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..s.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = s.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, s.elem_size, new_rank, dim_ptr, &mut stat);
        if stat != 0 || d.base_addr.is_null() {
            return;
        }
    }
    let dst_total = d.total_elements() as usize;
    let src_ptr = s.base_addr as *const u8;
    macro_rules! sum_dim_mask_kind {
        ($t:ty) => {{
            let buf = d.base_addr as *mut $t;
            for i in 0..dst_total {
                unsafe {
                    *buf.add(i) = 0;
                }
            }
            for_each_reduce_along_dim_with_mask(s, m, dim, |sb, mb, df| {
                if unsafe { mask_byte_is_true(m, mb) } {
                    let v = unsafe { *(src_ptr.add(sb) as *const $t) };
                    unsafe {
                        *buf.add(df) = (*buf.add(df)).wrapping_add(v);
                    }
                }
            });
        }};
    }
    match s.elem_size {
        1 => sum_dim_mask_kind!(i8),
        2 => sum_dim_mask_kind!(i16),
        8 => sum_dim_mask_kind!(i64),
        _ => sum_dim_mask_kind!(i32),
    }
}

/// SUM(array, DIM=k, MASK=mask) for complex(4).
#[no_mangle]
pub extern "C" fn afs_array_sum_complex4_dim_mask(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || mask.is_null() {
        return;
    }
    let s = unsafe { &*src };
    let d = unsafe { &mut *dst };
    let m = unsafe { &*mask };
    if !descriptor_has_payload_or_zero_size_array(s)
        || !descriptor_has_payload_or_zero_size_array(m)
    {
        return;
    }
    if !d.is_allocated() {
        let new_rank = (s.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..s.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = s.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, s.elem_size, new_rank, dim_ptr, &mut stat);
        if stat != 0 || d.base_addr.is_null() {
            return;
        }
    }
    let dst_total = d.total_elements() as usize;
    let buf = d.base_addr as *mut f32;
    for i in 0..(dst_total * 2) {
        unsafe {
            *buf.add(i) = 0.0;
        }
    }
    let src_ptr = s.base_addr as *const u8;
    for_each_reduce_along_dim_with_mask(s, m, dim, |sb, mb, df| {
        if unsafe { mask_byte_is_true(m, mb) } {
            let p = unsafe { src_ptr.add(sb) as *const f32 };
            unsafe {
                *buf.add(df * 2) += *p.add(0);
                *buf.add(df * 2 + 1) += *p.add(1);
            }
        }
    });
}

/// SUM(array, DIM=k, MASK=mask) for complex(8).
#[no_mangle]
pub extern "C" fn afs_array_sum_complex8_dim_mask(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || mask.is_null() {
        return;
    }
    let s = unsafe { &*src };
    let d = unsafe { &mut *dst };
    let m = unsafe { &*mask };
    if !descriptor_has_payload_or_zero_size_array(s)
        || !descriptor_has_payload_or_zero_size_array(m)
    {
        return;
    }
    if !d.is_allocated() {
        let new_rank = (s.rank - 1).max(0);
        let mut dim_buf: [DimDescriptor; 15] = [DimDescriptor {
            lower_bound: 0,
            upper_bound: 0,
            stride: 0,
        }; 15];
        let mut k = 0usize;
        let mut acc: i64 = 1;
        for i in 0..s.rank as usize {
            if i + 1 == dim as usize {
                continue;
            }
            let extent = s.dims[i].extent();
            dim_buf[k].lower_bound = 1;
            dim_buf[k].upper_bound = extent;
            dim_buf[k].stride = acc;
            acc *= extent;
            k += 1;
        }
        let dim_ptr = if new_rank > 0 {
            dim_buf.as_ptr()
        } else {
            ptr::null()
        };
        let mut stat: i32 = 0;
        afs_allocate_array(dst, s.elem_size, new_rank, dim_ptr, &mut stat);
        if stat != 0 || d.base_addr.is_null() {
            return;
        }
    }
    let dst_total = d.total_elements() as usize;
    let buf = d.base_addr as *mut f64;
    for i in 0..(dst_total * 2) {
        unsafe {
            *buf.add(i) = 0.0;
        }
    }
    let src_ptr = s.base_addr as *const u8;
    for_each_reduce_along_dim_with_mask(s, m, dim, |sb, mb, df| {
        if unsafe { mask_byte_is_true(m, mb) } {
            let p = unsafe { src_ptr.add(sb) as *const f64 };
            unsafe {
                *buf.add(df * 2) += *p.add(0);
                *buf.add(df * 2 + 1) += *p.add(1);
            }
        }
    });
}

/// SUM(array) — sum all elements (integer version).
/// Dispatches on `elem_size` so integer(1/2/4/8) arrays all sum correctly.
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
    let mut sum: i64 = 0;
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset(d, |byte_off| unsafe {
        sum = sum.wrapping_add(read_int_as_i64(src, byte_off, d.elem_size));
    });
    sum
}

fn for_each_element_byte_offset<F: FnMut(isize)>(desc: &ArrayDescriptor, mut f: F) {
    let rank = desc.rank as usize;
    if rank == 0 {
        return;
    }
    let mut extents: [i64; 15] = [0; 15];
    let mut strides: [i64; 15] = [0; 15];
    let mut total = 1i64;
    for i in 0..rank {
        extents[i] = desc.dims[i].extent();
        if extents[i] <= 0 {
            return;
        }
        strides[i] = desc.dims[i].stride;
        total *= extents[i];
    }

    let mut idx: [i64; 15] = [0; 15];
    let elem_size = desc.elem_size.max(1);
    for _ in 0..total {
        let mut byte_off = 0i64;
        for d in 0..rank {
            byte_off += idx[d] * strides[d] * elem_size;
        }
        f(byte_off as isize);

        for d in 0..rank {
            idx[d] += 1;
            if idx[d] < extents[d] {
                break;
            }
            idx[d] = 0;
        }
    }
}

fn for_each_element_byte_offset_with_mask<F: FnMut(isize)>(
    desc: &ArrayDescriptor,
    mask: &ArrayDescriptor,
    mut f: F,
) {
    let rank = desc.rank as usize;
    if rank == 0 {
        return;
    }
    let mut extents: [i64; 15] = [0; 15];
    let mut s_strides: [i64; 15] = [0; 15];
    let mut m_strides: [i64; 15] = [0; 15];
    let mut total = 1i64;
    for i in 0..rank {
        extents[i] = desc.dims[i].extent();
        if extents[i] <= 0 {
            return;
        }
        s_strides[i] = desc.dims[i].stride;
        m_strides[i] = if (i as i32) < mask.rank {
            mask.dims[i].stride
        } else {
            1
        };
        total *= extents[i];
    }

    let mut idx: [i64; 15] = [0; 15];
    let src_elem = desc.elem_size.max(1);
    let mask_elem = mask.elem_size.max(1);
    for _ in 0..total {
        let mut s_byte_off = 0i64;
        let mut m_byte_off = 0i64;
        for d in 0..rank {
            s_byte_off += idx[d] * s_strides[d] * src_elem;
            m_byte_off += idx[d] * m_strides[d] * mask_elem;
        }
        if unsafe { mask_byte_offset_is_true(mask, m_byte_off as isize) } {
            f(s_byte_off as isize);
        }

        for d in 0..rank {
            idx[d] += 1;
            if idx[d] < extents[d] {
                break;
            }
            idx[d] = 0;
        }
    }
}

unsafe fn read_real_as_f64(base: *const u8, byte_off: isize, elem_size: i64) -> f64 {
    let ptr = base.offset(byte_off);
    if elem_size == 4 {
        *(ptr as *const f32) as f64
    } else {
        *(ptr as *const f64)
    }
}

unsafe fn read_int_as_i64(base: *const u8, byte_off: isize, elem_size: i64) -> i64 {
    let ptr = base.offset(byte_off);
    match elem_size {
        1 => *(ptr as *const i8) as i64,
        2 => *(ptr as *const i16) as i64,
        8 => *(ptr as *const i64),
        _ => *(ptr as *const i32) as i64,
    }
}

unsafe fn mask_byte_offset_is_true(mask: &ArrayDescriptor, byte_off: isize) -> bool {
    let p = mask.base_addr.offset(byte_off);
    match mask.elem_size {
        1 => *p != 0,
        2 => *(p as *const u16) != 0,
        4 => *(p as *const u32) != 0,
        8 => *(p as *const u64) != 0,
        _ => *p != 0,
    }
}

/// SUM(array) for complex(4). The result is written to `out` as
/// `[real, imag]`.
#[no_mangle]
pub extern "C" fn afs_array_sum_complex4(out: *mut f32, desc: *const ArrayDescriptor) {
    if out.is_null() {
        return;
    }
    unsafe {
        *out.add(0) = 0.0;
        *out.add(1) = 0.0;
    }
    if desc.is_null() {
        return;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return;
    }
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset(d, |byte_off| unsafe {
        let p = src.offset(byte_off) as *const f32;
        *out.add(0) += *p.add(0);
        *out.add(1) += *p.add(1);
    });
}

/// SUM(array) for complex(8). The result is written to `out` as
/// `[real, imag]`.
#[no_mangle]
pub extern "C" fn afs_array_sum_complex8(out: *mut f64, desc: *const ArrayDescriptor) {
    if out.is_null() {
        return;
    }
    unsafe {
        *out.add(0) = 0.0;
        *out.add(1) = 0.0;
    }
    if desc.is_null() {
        return;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return;
    }
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset(d, |byte_off| unsafe {
        let p = src.offset(byte_off) as *const f64;
        *out.add(0) += *p.add(0);
        *out.add(1) += *p.add(1);
    });
}

fn int_maxval_identity(elem_size: i64) -> i64 {
    match elem_size {
        1 => i8::MIN as i64,
        2 => i16::MIN as i64,
        8 => i64::MIN,
        _ => i32::MIN as i64,
    }
}

fn int_minval_identity(elem_size: i64) -> i64 {
    match elem_size {
        1 => i8::MAX as i64,
        2 => i16::MAX as i64,
        8 => i64::MAX,
        _ => i32::MAX as i64,
    }
}

fn real_maxval_identity(elem_size: i64) -> f64 {
    if elem_size == 4 {
        -(f32::MAX as f64)
    } else {
        -f64::MAX
    }
}

fn real_minval_identity(elem_size: i64) -> f64 {
    if elem_size == 4 {
        f32::MAX as f64
    } else {
        f64::MAX
    }
}

/// SUM(array, mask=mask) — sum elements where `mask(i)` is true (real).
/// Width-dispatched on the array's elem_size; mask is read with its own
/// kind from `mask.elem_size`.
#[no_mangle]
pub extern "C" fn afs_array_sum_real8_mask(
    desc: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
) -> f64 {
    if desc.is_null() || mask.is_null() {
        return 0.0;
    }
    let d = unsafe { &*desc };
    let m = unsafe { &*mask };
    if d.base_addr.is_null() || m.base_addr.is_null() {
        return 0.0;
    }
    let mut sum: f64 = 0.0;
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset_with_mask(d, m, |byte_off| unsafe {
        sum += read_real_as_f64(src, byte_off, d.elem_size);
    });
    sum
}

/// SUM(array, mask=mask) — integer arrays. Dispatches on elem_size like
/// the unmasked entry; returns i64 for any input kind.
#[no_mangle]
pub extern "C" fn afs_array_sum_int_mask(
    desc: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
) -> i64 {
    if desc.is_null() || mask.is_null() {
        return 0;
    }
    let d = unsafe { &*desc };
    let mk = unsafe { &*mask };
    if d.base_addr.is_null() || mk.base_addr.is_null() {
        return 0;
    }
    let mut sum: i64 = 0;
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset_with_mask(d, mk, |byte_off| unsafe {
        sum = sum.wrapping_add(read_int_as_i64(src, byte_off, d.elem_size));
    });
    sum
}

/// SUM(array, mask=mask) for complex(4).
#[no_mangle]
pub extern "C" fn afs_array_sum_complex4_mask(
    out: *mut f32,
    desc: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
) {
    if out.is_null() {
        return;
    }
    unsafe {
        *out.add(0) = 0.0;
        *out.add(1) = 0.0;
    }
    if desc.is_null() || mask.is_null() {
        return;
    }
    let d = unsafe { &*desc };
    let m = unsafe { &*mask };
    if d.base_addr.is_null() || m.base_addr.is_null() {
        return;
    }
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset_with_mask(d, m, |byte_off| unsafe {
        let p = src.offset(byte_off) as *const f32;
        *out.add(0) += *p.add(0);
        *out.add(1) += *p.add(1);
    });
}

/// SUM(array, mask=mask) for complex(8).
#[no_mangle]
pub extern "C" fn afs_array_sum_complex8_mask(
    out: *mut f64,
    desc: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
) {
    if out.is_null() {
        return;
    }
    unsafe {
        *out.add(0) = 0.0;
        *out.add(1) = 0.0;
    }
    if desc.is_null() || mask.is_null() {
        return;
    }
    let d = unsafe { &*desc };
    let m = unsafe { &*mask };
    if d.base_addr.is_null() || m.base_addr.is_null() {
        return;
    }
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset_with_mask(d, m, |byte_off| unsafe {
        let p = src.offset(byte_off) as *const f64;
        *out.add(0) += *p.add(0);
        *out.add(1) += *p.add(1);
    });
}

/// PRODUCT(array) — product of all elements (real version).
/// Dispatches on `elem_size`; returns f64 for both real(4) and real(8).
#[no_mangle]
pub extern "C" fn afs_array_product_real8(desc: *const ArrayDescriptor) -> f64 {
    if desc.is_null() {
        return 1.0;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 1.0;
    }
    let mut prod: f64 = 1.0;
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset(d, |byte_off| unsafe {
        prod *= read_real_as_f64(src, byte_off, d.elem_size);
    });
    prod
}

/// PRODUCT(array) — product of all elements (integer version).
/// Dispatches on `elem_size` so integer(1/2/4/8) arrays multiply correctly.
#[no_mangle]
pub extern "C" fn afs_array_product_int(desc: *const ArrayDescriptor) -> i64 {
    if desc.is_null() {
        return 1;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 1;
    }
    let mut prod: i64 = 1;
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset(d, |byte_off| unsafe {
        prod = prod.wrapping_mul(read_int_as_i64(src, byte_off, d.elem_size));
    });
    prod
}

/// PRODUCT(array, DIM=k) — real version.
#[no_mangle]
pub extern "C" fn afs_array_product_real8_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    if !descriptor_has_payload_or_zero_size_array(s) || dim as usize > s.rank as usize {
        return;
    }
    if !ensure_reduction_dim_result(s, dim, dst, s.elem_size) {
        return;
    }
    let d = unsafe { &mut *dst };
    let dst_total = d.total_elements() as usize;
    let src_ptr = s.base_addr as *const u8;
    if s.elem_size == 4 {
        let buf = d.base_addr as *mut f32;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = 1.0;
            }
        }
        for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
            let v = unsafe { *(src_ptr.add(byte_off) as *const f32) };
            unsafe {
                *buf.add(dst_flat) *= v;
            }
        });
    } else {
        let buf = d.base_addr as *mut f64;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = 1.0;
            }
        }
        for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
            let v = unsafe { *(src_ptr.add(byte_off) as *const f64) };
            unsafe {
                *buf.add(dst_flat) *= v;
            }
        });
    }
}

/// PRODUCT(array, DIM=k) — integer version.
#[no_mangle]
pub extern "C" fn afs_array_product_int_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    if !descriptor_has_payload_or_zero_size_array(s) || dim as usize > s.rank as usize {
        return;
    }
    if !ensure_reduction_dim_result(s, dim, dst, s.elem_size) {
        return;
    }
    let d = unsafe { &mut *dst };
    let dst_total = d.total_elements() as usize;
    let src_ptr = s.base_addr as *const u8;
    macro_rules! product_dim_kind {
        ($t:ty) => {{
            let buf = d.base_addr as *mut $t;
            for i in 0..dst_total {
                unsafe {
                    *buf.add(i) = 1;
                }
            }
            for_each_reduce_along_dim(s, dim, |byte_off, dst_flat| {
                let v = unsafe { *(src_ptr.add(byte_off) as *const $t) };
                unsafe {
                    *buf.add(dst_flat) = (*buf.add(dst_flat)).wrapping_mul(v);
                }
            });
        }};
    }
    match s.elem_size {
        1 => product_dim_kind!(i8),
        2 => product_dim_kind!(i16),
        8 => product_dim_kind!(i64),
        _ => product_dim_kind!(i32),
    }
}

/// PRODUCT(array, DIM=k, MASK=mask) — real version.
#[no_mangle]
pub extern "C" fn afs_array_product_real8_dim_mask(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || mask.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    let m = unsafe { &*mask };
    if !descriptor_has_payload_or_zero_size_array(s)
        || !descriptor_has_payload_or_zero_size_array(m)
        || dim as usize > s.rank as usize
    {
        return;
    }
    if !ensure_reduction_dim_result(s, dim, dst, s.elem_size) {
        return;
    }
    let d = unsafe { &mut *dst };
    let dst_total = d.total_elements() as usize;
    let src_ptr = s.base_addr as *const u8;
    if s.elem_size == 4 {
        let buf = d.base_addr as *mut f32;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = 1.0;
            }
        }
        for_each_reduce_along_dim_with_mask(s, m, dim, |sb, mb, df| {
            if unsafe { mask_byte_is_true(m, mb) } {
                let v = unsafe { *(src_ptr.add(sb) as *const f32) };
                unsafe {
                    *buf.add(df) *= v;
                }
            }
        });
    } else {
        let buf = d.base_addr as *mut f64;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = 1.0;
            }
        }
        for_each_reduce_along_dim_with_mask(s, m, dim, |sb, mb, df| {
            if unsafe { mask_byte_is_true(m, mb) } {
                let v = unsafe { *(src_ptr.add(sb) as *const f64) };
                unsafe {
                    *buf.add(df) *= v;
                }
            }
        });
    }
}

/// PRODUCT(array, DIM=k, MASK=mask) — integer version.
#[no_mangle]
pub extern "C" fn afs_array_product_int_dim_mask(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || mask.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    let m = unsafe { &*mask };
    if !descriptor_has_payload_or_zero_size_array(s)
        || !descriptor_has_payload_or_zero_size_array(m)
        || dim as usize > s.rank as usize
    {
        return;
    }
    if !ensure_reduction_dim_result(s, dim, dst, s.elem_size) {
        return;
    }
    let d = unsafe { &mut *dst };
    let dst_total = d.total_elements() as usize;
    let src_ptr = s.base_addr as *const u8;
    macro_rules! product_dim_mask_kind {
        ($t:ty) => {{
            let buf = d.base_addr as *mut $t;
            for i in 0..dst_total {
                unsafe {
                    *buf.add(i) = 1;
                }
            }
            for_each_reduce_along_dim_with_mask(s, m, dim, |sb, mb, df| {
                if unsafe { mask_byte_is_true(m, mb) } {
                    let v = unsafe { *(src_ptr.add(sb) as *const $t) };
                    unsafe {
                        *buf.add(df) = (*buf.add(df)).wrapping_mul(v);
                    }
                }
            });
        }};
    }
    match s.elem_size {
        1 => product_dim_mask_kind!(i8),
        2 => product_dim_mask_kind!(i16),
        8 => product_dim_mask_kind!(i64),
        _ => product_dim_mask_kind!(i32),
    }
}

/// PRODUCT(array, mask=mask) — masked product (real). Dispatches on
/// elem_size and reads the mask using its own descriptor strides.
#[no_mangle]
pub extern "C" fn afs_array_product_real8_mask(
    desc: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
) -> f64 {
    if desc.is_null() || mask.is_null() {
        return 1.0;
    }
    let d = unsafe { &*desc };
    let m = unsafe { &*mask };
    if d.base_addr.is_null() || m.base_addr.is_null() {
        return 1.0;
    }
    let mut prod: f64 = 1.0;
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset_with_mask(d, m, |byte_off| unsafe {
        prod *= read_real_as_f64(src, byte_off, d.elem_size);
    });
    prod
}

/// PRODUCT(array, mask=mask) — masked product (integer). Dispatches on
/// elem_size; returns i64 regardless of input kind.
#[no_mangle]
pub extern "C" fn afs_array_product_int_mask(
    desc: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
) -> i64 {
    if desc.is_null() || mask.is_null() {
        return 1;
    }
    let d = unsafe { &*desc };
    let mk = unsafe { &*mask };
    if d.base_addr.is_null() || mk.base_addr.is_null() {
        return 1;
    }
    let mut prod: i64 = 1;
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset_with_mask(d, mk, |byte_off| unsafe {
        prod = prod.wrapping_mul(read_int_as_i64(src, byte_off, d.elem_size));
    });
    prod
}

/// MAXVAL(array, mask=mask) — masked max (real).
#[no_mangle]
pub extern "C" fn afs_array_maxval_real8_mask(
    desc: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
) -> f64 {
    if desc.is_null() {
        return -f64::MAX;
    }
    let d = unsafe { &*desc };
    let identity = real_maxval_identity(d.elem_size);
    if mask.is_null() {
        return identity;
    }
    let m = unsafe { &*mask };
    if d.base_addr.is_null() || m.base_addr.is_null() {
        return identity;
    }
    let mut best = identity;
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset_with_mask(d, m, |byte_off| unsafe {
        let value = read_real_as_f64(src, byte_off, d.elem_size);
        if value > best {
            best = value;
        }
    });
    best
}

/// MINVAL(array, mask=mask) — masked min (real).
#[no_mangle]
pub extern "C" fn afs_array_minval_real8_mask(
    desc: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
) -> f64 {
    if desc.is_null() {
        return f64::MAX;
    }
    let d = unsafe { &*desc };
    let identity = real_minval_identity(d.elem_size);
    if mask.is_null() {
        return identity;
    }
    let m = unsafe { &*mask };
    if d.base_addr.is_null() || m.base_addr.is_null() {
        return identity;
    }
    let mut best = identity;
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset_with_mask(d, m, |byte_off| unsafe {
        let value = read_real_as_f64(src, byte_off, d.elem_size);
        if value < best {
            best = value;
        }
    });
    best
}

/// MAXVAL(array, mask=mask) — masked max (integer).
#[no_mangle]
pub extern "C" fn afs_array_maxval_int_mask(
    desc: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
) -> i64 {
    if desc.is_null() {
        return i64::MIN;
    }
    let d = unsafe { &*desc };
    let identity = int_maxval_identity(d.elem_size);
    if mask.is_null() {
        return identity;
    }
    let mk = unsafe { &*mask };
    if d.base_addr.is_null() || mk.base_addr.is_null() {
        return identity;
    }
    let mut best = identity;
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset_with_mask(d, mk, |byte_off| unsafe {
        let value = read_int_as_i64(src, byte_off, d.elem_size);
        if value > best {
            best = value;
        }
    });
    best
}

/// MINVAL(array, mask=mask) — masked min (integer).
#[no_mangle]
pub extern "C" fn afs_array_minval_int_mask(
    desc: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
) -> i64 {
    if desc.is_null() {
        return i64::MAX;
    }
    let d = unsafe { &*desc };
    let identity = int_minval_identity(d.elem_size);
    if mask.is_null() {
        return identity;
    }
    let mk = unsafe { &*mask };
    if d.base_addr.is_null() || mk.base_addr.is_null() {
        return identity;
    }
    let mut best = identity;
    let src = d.base_addr as *const u8;
    for_each_element_byte_offset_with_mask(d, mk, |byte_off| unsafe {
        let value = read_int_as_i64(src, byte_off, d.elem_size);
        if value < best {
            best = value;
        }
    });
    best
}

/// MAXVAL(array, DIM=k, MASK=mask) — real version.
#[no_mangle]
pub extern "C" fn afs_array_maxval_real8_dim_mask(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || mask.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    let m = unsafe { &*mask };
    if !descriptor_has_payload_or_zero_size_array(s)
        || !descriptor_has_payload_or_zero_size_array(m)
        || dim as usize > s.rank as usize
    {
        return;
    }
    if !ensure_reduction_dim_result(s, dim, dst, s.elem_size) {
        return;
    }
    let d = unsafe { &mut *dst };
    let dst_total = d.total_elements() as usize;
    let src_ptr = s.base_addr as *const u8;
    if s.elem_size == 4 {
        let buf = d.base_addr as *mut f32;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = -f32::MAX;
            }
        }
        for_each_reduce_along_dim_with_mask(s, m, dim, |sb, mb, df| {
            if unsafe { mask_byte_is_true(m, mb) } {
                let v = unsafe { *(src_ptr.add(sb) as *const f32) };
                unsafe {
                    let slot = buf.add(df);
                    if v > *slot {
                        *slot = v;
                    }
                }
            }
        });
    } else {
        let buf = d.base_addr as *mut f64;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = -f64::MAX;
            }
        }
        for_each_reduce_along_dim_with_mask(s, m, dim, |sb, mb, df| {
            if unsafe { mask_byte_is_true(m, mb) } {
                let v = unsafe { *(src_ptr.add(sb) as *const f64) };
                unsafe {
                    let slot = buf.add(df);
                    if v > *slot {
                        *slot = v;
                    }
                }
            }
        });
    }
}

/// MINVAL(array, DIM=k, MASK=mask) — real version.
#[no_mangle]
pub extern "C" fn afs_array_minval_real8_dim_mask(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || mask.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    let m = unsafe { &*mask };
    if !descriptor_has_payload_or_zero_size_array(s)
        || !descriptor_has_payload_or_zero_size_array(m)
        || dim as usize > s.rank as usize
    {
        return;
    }
    if !ensure_reduction_dim_result(s, dim, dst, s.elem_size) {
        return;
    }
    let d = unsafe { &mut *dst };
    let dst_total = d.total_elements() as usize;
    let src_ptr = s.base_addr as *const u8;
    if s.elem_size == 4 {
        let buf = d.base_addr as *mut f32;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = f32::MAX;
            }
        }
        for_each_reduce_along_dim_with_mask(s, m, dim, |sb, mb, df| {
            if unsafe { mask_byte_is_true(m, mb) } {
                let v = unsafe { *(src_ptr.add(sb) as *const f32) };
                unsafe {
                    let slot = buf.add(df);
                    if v < *slot {
                        *slot = v;
                    }
                }
            }
        });
    } else {
        let buf = d.base_addr as *mut f64;
        for i in 0..dst_total {
            unsafe {
                *buf.add(i) = f64::MAX;
            }
        }
        for_each_reduce_along_dim_with_mask(s, m, dim, |sb, mb, df| {
            if unsafe { mask_byte_is_true(m, mb) } {
                let v = unsafe { *(src_ptr.add(sb) as *const f64) };
                unsafe {
                    let slot = buf.add(df);
                    if v < *slot {
                        *slot = v;
                    }
                }
            }
        });
    }
}

/// MAXVAL(array, DIM=k, MASK=mask) — integer version.
#[no_mangle]
pub extern "C" fn afs_array_maxval_int_dim_mask(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || mask.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    let m = unsafe { &*mask };
    if !descriptor_has_payload_or_zero_size_array(s)
        || !descriptor_has_payload_or_zero_size_array(m)
        || dim as usize > s.rank as usize
    {
        return;
    }
    if !ensure_reduction_dim_result(s, dim, dst, s.elem_size) {
        return;
    }
    let d = unsafe { &mut *dst };
    let dst_total = d.total_elements() as usize;
    let src_ptr = s.base_addr as *const u8;
    macro_rules! max_dim_mask_kind {
        ($t:ty, $identity:expr) => {{
            let buf = d.base_addr as *mut $t;
            for i in 0..dst_total {
                unsafe {
                    *buf.add(i) = $identity;
                }
            }
            for_each_reduce_along_dim_with_mask(s, m, dim, |sb, mb, df| {
                if unsafe { mask_byte_is_true(m, mb) } {
                    let v = unsafe { *(src_ptr.add(sb) as *const $t) };
                    unsafe {
                        let slot = buf.add(df);
                        if v > *slot {
                            *slot = v;
                        }
                    }
                }
            });
        }};
    }
    match s.elem_size {
        1 => max_dim_mask_kind!(i8, i8::MIN),
        2 => max_dim_mask_kind!(i16, i16::MIN),
        8 => max_dim_mask_kind!(i64, i64::MIN),
        _ => max_dim_mask_kind!(i32, i32::MIN),
    }
}

/// MINVAL(array, DIM=k, MASK=mask) — integer version.
#[no_mangle]
pub extern "C" fn afs_array_minval_int_dim_mask(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
) {
    if src.is_null() || dst.is_null() || mask.is_null() || dim < 1 {
        return;
    }
    let s = unsafe { &*src };
    let m = unsafe { &*mask };
    if !descriptor_has_payload_or_zero_size_array(s)
        || !descriptor_has_payload_or_zero_size_array(m)
        || dim as usize > s.rank as usize
    {
        return;
    }
    if !ensure_reduction_dim_result(s, dim, dst, s.elem_size) {
        return;
    }
    let d = unsafe { &mut *dst };
    let dst_total = d.total_elements() as usize;
    let src_ptr = s.base_addr as *const u8;
    macro_rules! min_dim_mask_kind {
        ($t:ty, $identity:expr) => {{
            let buf = d.base_addr as *mut $t;
            for i in 0..dst_total {
                unsafe {
                    *buf.add(i) = $identity;
                }
            }
            for_each_reduce_along_dim_with_mask(s, m, dim, |sb, mb, df| {
                if unsafe { mask_byte_is_true(m, mb) } {
                    let v = unsafe { *(src_ptr.add(sb) as *const $t) };
                    unsafe {
                        let slot = buf.add(df);
                        if v < *slot {
                            *slot = v;
                        }
                    }
                }
            });
        }};
    }
    match s.elem_size {
        1 => min_dim_mask_kind!(i8, i8::MAX),
        2 => min_dim_mask_kind!(i16, i16::MAX),
        8 => min_dim_mask_kind!(i64, i64::MAX),
        _ => min_dim_mask_kind!(i32, i32::MAX),
    }
}

/// MAXVAL(array) — maximum element (real version). Dispatches on
/// `elem_size`; returns f64 for both real(4) and real(8).
/// Respects strides.
#[no_mangle]
pub extern "C" fn afs_array_maxval_real8(desc: *const ArrayDescriptor) -> f64 {
    if desc.is_null() {
        return -f64::MAX;
    }
    let d = unsafe { &*desc };
    let identity = real_maxval_identity(d.elem_size);
    if d.base_addr.is_null() {
        return identity;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return identity;
    }
    let src = d.base_addr as *const u8;
    let mut max = identity;
    for_each_element_byte_offset(d, |byte_off| unsafe {
        let v = read_real_as_f64(src, byte_off, d.elem_size);
        if v > max {
            max = v;
        }
    });
    max
}

/// MINVAL(array) — minimum element (real version). Dispatches on
/// `elem_size`; returns f64 for both real(4) and real(8).
/// Respects strides.
#[no_mangle]
pub extern "C" fn afs_array_minval_real8(desc: *const ArrayDescriptor) -> f64 {
    if desc.is_null() {
        return f64::MAX;
    }
    let d = unsafe { &*desc };
    let identity = real_minval_identity(d.elem_size);
    if d.base_addr.is_null() {
        return identity;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return identity;
    }
    let src = d.base_addr as *const u8;
    let mut min = identity;
    for_each_element_byte_offset(d, |byte_off| unsafe {
        let v = read_real_as_f64(src, byte_off, d.elem_size);
        if v < min {
            min = v;
        }
    });
    min
}

/// MAXVAL(array) — maximum element (integer version).
/// Dispatches on `elem_size` so integer(1/2/4/8) arrays read correctly.
/// Returns i64 so all kinds fit; codegen truncates to result kind.
#[no_mangle]
pub extern "C" fn afs_array_maxval_int(desc: *const ArrayDescriptor) -> i64 {
    if desc.is_null() {
        return i64::MIN;
    }
    let d = unsafe { &*desc };
    let identity = int_maxval_identity(d.elem_size);
    if d.base_addr.is_null() {
        return identity;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return identity;
    }
    let src = d.base_addr as *const u8;
    let mut max = identity;
    for_each_element_byte_offset(d, |byte_off| unsafe {
        let v = read_int_as_i64(src, byte_off, d.elem_size);
        if v > max {
            max = v;
        }
    });
    max
}

/// MINVAL(array) — minimum element (integer version).
/// Dispatches on `elem_size` so integer(1/2/4/8) arrays read correctly.
/// Returns i64 so all kinds fit; codegen truncates to result kind.
#[no_mangle]
pub extern "C" fn afs_array_minval_int(desc: *const ArrayDescriptor) -> i64 {
    if desc.is_null() {
        return i64::MAX;
    }
    let d = unsafe { &*desc };
    let identity = int_minval_identity(d.elem_size);
    if d.base_addr.is_null() {
        return identity;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return identity;
    }
    let src = d.base_addr as *const u8;
    let mut min = identity;
    for_each_element_byte_offset(d, |byte_off| unsafe {
        let v = read_int_as_i64(src, byte_off, d.elem_size);
        if v < min {
            min = v;
        }
    });
    min
}

/// MAXLOC(array, dim=1) for rank-1 input — returns 1-based index of the
/// maximum element (real(4)). F2018 §16.9.130. Dispatches on elem_size.
#[no_mangle]
pub extern "C" fn afs_array_maxloc_real4(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() {
        return 0;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 0;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return 0;
    }
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const f32;
    let mut max = unsafe { *ptr };
    let mut idx = 0usize;
    for i in 1..n {
        let v = unsafe { *ptr.add(i * stride) };
        if v > max {
            max = v;
            idx = i;
        }
    }
    (idx as i32) + 1
}

#[no_mangle]
pub extern "C" fn afs_array_maxloc_real8(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() {
        return 0;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 0;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return 0;
    }
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const f64;
    let mut max = unsafe { *ptr };
    let mut idx = 0usize;
    for i in 1..n {
        let v = unsafe { *ptr.add(i * stride) };
        if v > max {
            max = v;
            idx = i;
        }
    }
    (idx as i32) + 1
}

#[no_mangle]
pub extern "C" fn afs_array_maxloc_int(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() {
        return 0;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 0;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return 0;
    }
    let stride = d.dims[0].stride.max(1) as usize;
    let mut idx = 0usize;
    match d.elem_size {
        1 => {
            let ptr = d.base_addr as *const i8;
            let mut max = unsafe { *ptr };
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) };
                if v > max {
                    max = v;
                    idx = i;
                }
            }
        }
        2 => {
            let ptr = d.base_addr as *const i16;
            let mut max = unsafe { *ptr };
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) };
                if v > max {
                    max = v;
                    idx = i;
                }
            }
        }
        8 => {
            let ptr = d.base_addr as *const i64;
            let mut max = unsafe { *ptr };
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) };
                if v > max {
                    max = v;
                    idx = i;
                }
            }
        }
        _ => {
            let ptr = d.base_addr as *const i32;
            let mut max = unsafe { *ptr };
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) };
                if v > max {
                    max = v;
                    idx = i;
                }
            }
        }
    }
    (idx as i32) + 1
}

/// MINLOC(array) for rank-1 input — analogous to MAXLOC.
#[no_mangle]
pub extern "C" fn afs_array_minloc_real4(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() {
        return 0;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 0;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return 0;
    }
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const f32;
    let mut min = unsafe { *ptr };
    let mut idx = 0usize;
    for i in 1..n {
        let v = unsafe { *ptr.add(i * stride) };
        if v < min {
            min = v;
            idx = i;
        }
    }
    (idx as i32) + 1
}

#[no_mangle]
pub extern "C" fn afs_array_minloc_real8(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() {
        return 0;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 0;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return 0;
    }
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const f64;
    let mut min = unsafe { *ptr };
    let mut idx = 0usize;
    for i in 1..n {
        let v = unsafe { *ptr.add(i * stride) };
        if v < min {
            min = v;
            idx = i;
        }
    }
    (idx as i32) + 1
}

#[no_mangle]
pub extern "C" fn afs_array_minloc_int(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() {
        return 0;
    }
    let d = unsafe { &*desc };
    if d.base_addr.is_null() {
        return 0;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return 0;
    }
    let stride = d.dims[0].stride.max(1) as usize;
    let mut idx = 0usize;
    match d.elem_size {
        1 => {
            let ptr = d.base_addr as *const i8;
            let mut min = unsafe { *ptr };
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) };
                if v < min {
                    min = v;
                    idx = i;
                }
            }
        }
        2 => {
            let ptr = d.base_addr as *const i16;
            let mut min = unsafe { *ptr };
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) };
                if v < min {
                    min = v;
                    idx = i;
                }
            }
        }
        8 => {
            let ptr = d.base_addr as *const i64;
            let mut min = unsafe { *ptr };
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) };
                if v < min {
                    min = v;
                    idx = i;
                }
            }
        }
        _ => {
            let ptr = d.base_addr as *const i32;
            let mut min = unsafe { *ptr };
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) };
                if v < min {
                    min = v;
                    idx = i;
                }
            }
        }
    }
    (idx as i32) + 1
}

#[no_mangle]
pub extern "C" fn afs_array_maxloc_real4_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    array_loc_real_dim(src, dim, dst, true);
}

#[no_mangle]
pub extern "C" fn afs_array_maxloc_real8_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    array_loc_real_dim(src, dim, dst, true);
}

#[no_mangle]
pub extern "C" fn afs_array_maxloc_int_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    array_loc_int_dim(src, dim, dst, true);
}

#[no_mangle]
pub extern "C" fn afs_array_minloc_real4_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    array_loc_real_dim(src, dim, dst, false);
}

#[no_mangle]
pub extern "C" fn afs_array_minloc_real8_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    array_loc_real_dim(src, dim, dst, false);
}

#[no_mangle]
pub extern "C" fn afs_array_minloc_int_dim(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
) {
    array_loc_int_dim(src, dim, dst, false);
}

#[no_mangle]
pub extern "C" fn afs_array_maxloc_real4_dim_mask_back(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
    mask_scalar: i32,
    back: i32,
) {
    array_loc_real_dim_keywords(src, dim, dst, mask, mask_scalar, back, true);
}

#[no_mangle]
pub extern "C" fn afs_array_maxloc_real8_dim_mask_back(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
    mask_scalar: i32,
    back: i32,
) {
    array_loc_real_dim_keywords(src, dim, dst, mask, mask_scalar, back, true);
}

#[no_mangle]
pub extern "C" fn afs_array_maxloc_int_dim_mask_back(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
    mask_scalar: i32,
    back: i32,
) {
    array_loc_int_dim_keywords(src, dim, dst, mask, mask_scalar, back, true);
}

#[no_mangle]
pub extern "C" fn afs_array_minloc_real4_dim_mask_back(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
    mask_scalar: i32,
    back: i32,
) {
    array_loc_real_dim_keywords(src, dim, dst, mask, mask_scalar, back, false);
}

#[no_mangle]
pub extern "C" fn afs_array_minloc_real8_dim_mask_back(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
    mask_scalar: i32,
    back: i32,
) {
    array_loc_real_dim_keywords(src, dim, dst, mask, mask_scalar, back, false);
}

#[no_mangle]
pub extern "C" fn afs_array_minloc_int_dim_mask_back(
    src: *const ArrayDescriptor,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
    mask_scalar: i32,
    back: i32,
) {
    array_loc_int_dim_keywords(src, dim, dst, mask, mask_scalar, back, false);
}

#[no_mangle]
pub extern "C" fn afs_array_findloc_real8_dim_mask_back(
    src: *const ArrayDescriptor,
    value: f64,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
    mask_scalar: i32,
    back: i32,
) {
    array_findloc_real_dim_keywords(src, value, dim, dst, mask, mask_scalar, back);
}

#[no_mangle]
pub extern "C" fn afs_array_findloc_int_dim_mask_back(
    src: *const ArrayDescriptor,
    value: i64,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
    mask_scalar: i32,
    back: i32,
) {
    array_findloc_int_dim_keywords(src, value, dim, dst, mask, mask_scalar, back);
}

#[no_mangle]
pub extern "C" fn afs_array_findloc_logical_dim_mask_back(
    src: *const ArrayDescriptor,
    value: i32,
    dim: i32,
    dst: *mut ArrayDescriptor,
    mask: *const ArrayDescriptor,
    mask_scalar: i32,
    back: i32,
) {
    array_findloc_logical_dim_keywords(src, value, dim, dst, mask, mask_scalar, back);
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
    if src.rank < 2 {
        return;
    }

    let m = src.dims[0].extent() as usize;
    let n = src.dims[1].extent() as usize;
    let elem_size = src.elem_size.max(1);

    // Allocate result as (n x m) using the source's element width so
    // real(4) and real(8) (and complex(4)/(8) when routed here) all
    // get the right buffer size and stride.
    afs_allocate_1d(result, elem_size, (n * m) as i64);
    let res = unsafe { &mut *result };
    set_rank2_contiguous_shape(res, n, m);
    if m == 0 || n == 0 || src.base_addr.is_null() || res.base_addr.is_null() {
        return;
    }

    // Fortran arrays are column-major: source A(i,j) at offset j*m+i for
    // an m-row source; result B = transpose(A) has n rows, so B(j,i) at
    // offset i*n+j. The previous formulas were swapped (rp[j*m+i] =
    // sp[i*n+j]) which used row-major indexing on both sides; for any
    // non-square source this produced a scrambled output that's neither
    // the transpose nor the original. Surfaced in stdlib_stats cov_2_*
    // where `matmul(transpose(center), center)` returned all zeros — the
    // mis-strided transpose left the matrix multiply consuming the wrong
    // lanes, and the elements summed to 0 by accident on the toy input.
    if elem_size == 4 {
        let sp = src.base_addr as *const f32;
        let rp = res.base_addr as *mut f32;
        for i in 0..m {
            for j in 0..n {
                unsafe {
                    *rp.add(i * n + j) = *sp.add(j * m + i);
                }
            }
        }
    } else if elem_size == 8 {
        let sp = src.base_addr as *const f64;
        let rp = res.base_addr as *mut f64;
        for i in 0..m {
            for j in 0..n {
                unsafe {
                    *rp.add(i * n + j) = *sp.add(j * m + i);
                }
            }
        }
    } else {
        // Generic byte-level copy for other widths (complex(4)=8 already
        // handled above as f64 lanes; complex(8)=16 falls here).
        let sb = elem_size as usize;
        let sp = src.base_addr;
        let rp = res.base_addr;
        for i in 0..m {
            for j in 0..n {
                unsafe {
                    let src_off = (j * m + i) * sb;
                    let dst_off = (i * n + j) * sb;
                    core::ptr::copy_nonoverlapping(sp.add(src_off), rp.add(dst_off), sb);
                }
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

    let Some(shape) = matmul_shape(da, db) else {
        return;
    };
    let MatmulShape { m, k, n, .. } = shape;
    let elem_size = da.elem_size.max(1);

    // Allocate result using the source element width so real(4) and
    // real(8) inputs both produce correctly-sized output buffers.
    allocate_matmul_result(result, elem_size, shape);
    let res = unsafe { &mut *result };

    // Fortran is column-major: A(m,k) stores A(i,l) at l*m + i,
    // B(k,n) stores B(l,j) at j*k + l, C(m,n) stores C(i,j) at j*m + i.
    if elem_size == 4 {
        let ap = da.base_addr as *const f32;
        let bp = db.base_addr as *const f32;
        let rp = res.base_addr as *mut f32;
        for j in 0..n {
            for i in 0..m {
                let mut sum: f64 = 0.0;
                for l in 0..k {
                    let a_val = unsafe { *ap.add(l * m + i) } as f64;
                    let b_val = unsafe { *bp.add(j * k + l) } as f64;
                    sum += a_val * b_val;
                }
                unsafe {
                    *rp.add(j * m + i) = sum as f32;
                }
            }
        }
    } else {
        let ap = da.base_addr as *const f64;
        let bp = db.base_addr as *const f64;
        let rp = res.base_addr as *mut f64;
        for j in 0..n {
            for i in 0..m {
                let mut sum: f64 = 0.0;
                for l in 0..k {
                    let a_val = unsafe { *ap.add(l * m + i) };
                    let b_val = unsafe { *bp.add(j * k + l) };
                    sum += a_val * b_val;
                }
                unsafe {
                    *rp.add(j * m + i) = sum;
                }
            }
        }
    }
}

/// MATMUL(a, b, result) — matrix multiplication (complex version).
/// elem_size 8 → complex(4) (two f32 lanes); elem_size 16 → complex(8) (two f64 lanes).
#[no_mangle]
pub extern "C" fn afs_matmul_complex(
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

    let Some(shape) = matmul_shape(da, db) else {
        return;
    };
    let MatmulShape { m, k, n, .. } = shape;
    let elem_size = da.elem_size.max(8);

    allocate_matmul_result(result, elem_size, shape);
    let res = unsafe { &mut *result };

    if elem_size == 8 {
        // complex(4): pairs of f32 (re, im).
        let ap = da.base_addr as *const f32;
        let bp = db.base_addr as *const f32;
        let rp = res.base_addr as *mut f32;
        for j in 0..n {
            for i in 0..m {
                let mut sum_re: f64 = 0.0;
                let mut sum_im: f64 = 0.0;
                for l in 0..k {
                    let a_re = unsafe { *ap.add(2 * (l * m + i)) } as f64;
                    let a_im = unsafe { *ap.add(2 * (l * m + i) + 1) } as f64;
                    let b_re = unsafe { *bp.add(2 * (j * k + l)) } as f64;
                    let b_im = unsafe { *bp.add(2 * (j * k + l) + 1) } as f64;
                    sum_re += a_re * b_re - a_im * b_im;
                    sum_im += a_re * b_im + a_im * b_re;
                }
                unsafe {
                    *rp.add(2 * (j * m + i)) = sum_re as f32;
                    *rp.add(2 * (j * m + i) + 1) = sum_im as f32;
                }
            }
        }
    } else {
        // complex(8): pairs of f64 (re, im).
        let ap = da.base_addr as *const f64;
        let bp = db.base_addr as *const f64;
        let rp = res.base_addr as *mut f64;
        for j in 0..n {
            for i in 0..m {
                let mut sum_re: f64 = 0.0;
                let mut sum_im: f64 = 0.0;
                for l in 0..k {
                    let a_re = unsafe { *ap.add(2 * (l * m + i)) };
                    let a_im = unsafe { *ap.add(2 * (l * m + i) + 1) };
                    let b_re = unsafe { *bp.add(2 * (j * k + l)) };
                    let b_im = unsafe { *bp.add(2 * (j * k + l) + 1) };
                    sum_re += a_re * b_re - a_im * b_im;
                    sum_im += a_re * b_im + a_im * b_re;
                }
                unsafe {
                    *rp.add(2 * (j * m + i)) = sum_re;
                    *rp.add(2 * (j * m + i) + 1) = sum_im;
                }
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

    let Some(shape) = matmul_shape(da, db) else {
        return;
    };
    let MatmulShape { m, k, n, .. } = shape;

    let ap = da.base_addr as *const i32;
    let bp = db.base_addr as *const i32;

    allocate_matmul_result(result, 4, shape);
    let res = unsafe { &mut *result };
    let rp = res.base_addr as *mut i32;

    // Fortran is column-major: A(m,k) stores A(i,l) at l*m + i,
    // B(k,n) stores B(l,j) at j*k + l, C(m,n) stores C(i,j) at j*m + i.
    for j in 0..n {
        for i in 0..m {
            let mut sum: i64 = 0;
            for l in 0..k {
                let a_val = unsafe { *ap.add(l * m + i) as i64 };
                let b_val = unsafe { *bp.add(j * k + l) as i64 };
                sum += a_val * b_val;
            }
            unsafe {
                *rp.add(j * m + i) = sum as i32;
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
    if src.rank < 2 {
        return;
    }

    let m = src.dims[0].extent() as usize;
    let n = src.dims[1].extent() as usize;
    let elem_size = src.elem_size.max(1) as usize;

    // Allocate result with same per-element width so callers using
    // complex (8/16-byte), integer(8) (8-byte), integer(2)/(1) etc. all
    // round-trip without truncation. The previous always-i32 path silently
    // dropped the upper bytes of every element for non-32-bit types.
    let dim0 = DimDescriptor {
        lower_bound: 1,
        upper_bound: n as i64,
        stride: 1,
    };
    let dim1 = DimDescriptor {
        lower_bound: 1,
        upper_bound: m as i64,
        stride: 1,
    };
    let dims = [dim0, dim1];
    afs_allocate_array(result, elem_size as i64, 2, dims.as_ptr(), ptr::null_mut());
    let res = unsafe { &mut *result };
    if m == 0 || n == 0 || src.base_addr.is_null() || res.base_addr.is_null() {
        return;
    }
    let sp = src.base_addr as *const u8;
    let rp = res.base_addr;

    // Column-major: source A(i,j) at offset j*m+i; dest B(j,i) at i*n+j.
    // See afs_transpose_real8 for the full root-cause note.
    for i in 0..m {
        for j in 0..n {
            let src_off = (j * m + i) * elem_size;
            let dst_off = (i * n + j) * elem_size;
            unsafe {
                core::ptr::copy_nonoverlapping(sp.add(src_off), rp.add(dst_off), elem_size);
            }
        }
    }
}

/// CONJG over a complex array: allocate result with the same shape and
/// element size, copy the real lane verbatim and negate the imag lane.
/// Handles complex(sp) (8-byte) and complex(dp) (16-byte) by reading the
/// per-element width from the descriptor.
#[no_mangle]
pub extern "C" fn afs_array_conjg(source: *const ArrayDescriptor, result: *mut ArrayDescriptor) {
    if source.is_null() || result.is_null() {
        return;
    }
    let src = unsafe { &*source };
    if src.base_addr.is_null() {
        return;
    }
    afs_allocate_like(result, source, ptr::null_mut());
    let res = unsafe { &mut *result };
    let elem_size = src.elem_size.max(1) as usize;
    let lane = elem_size / 2;
    let total = src.total_elements() as usize;
    let sp = src.base_addr as *const u8;
    let rp = res.base_addr;
    if elem_size == 8 {
        // complex(sp): two f32 lanes per element
        for i in 0..total {
            let off = i * 8;
            unsafe {
                let re = *(sp.add(off) as *const f32);
                let im = *(sp.add(off + lane) as *const f32);
                *(rp.add(off) as *mut f32) = re;
                *(rp.add(off + lane) as *mut f32) = -im;
            }
        }
    } else if elem_size == 16 {
        // complex(dp): two f64 lanes per element
        for i in 0..total {
            let off = i * 16;
            unsafe {
                let re = *(sp.add(off) as *const f64);
                let im = *(sp.add(off + lane) as *const f64);
                *(rp.add(off) as *mut f64) = re;
                *(rp.add(off + lane) as *mut f64) = -im;
            }
        }
    } else {
        // Non-complex element width: byte-copy (degenerates to identity).
        for i in 0..total {
            let off = i * elem_size;
            unsafe {
                core::ptr::copy_nonoverlapping(sp.add(off), rp.add(off), elem_size);
            }
        }
    }
}

/// AIMAG over a complex array: produce a real array of the same shape
/// whose elements are the imaginary lanes of the source. Result has
/// HALF the source elem_size (complex(sp) 8B → real(sp) 4B; complex(dp)
/// 16B → real(dp) 8B), so we allocate fresh dims rather than using
/// `afs_allocate_like`.
#[no_mangle]
pub extern "C" fn afs_array_aimag(source: *const ArrayDescriptor, result: *mut ArrayDescriptor) {
    if source.is_null() || result.is_null() {
        return;
    }
    let src = unsafe { &*source };
    if src.base_addr.is_null() {
        return;
    }
    let elem_size = src.elem_size.max(1) as usize;
    let lane = elem_size / 2;
    let mut dims = [DimDescriptor::default(); MAX_RANK];
    for (i, dim) in dims.iter_mut().enumerate().take(src.rank as usize) {
        *dim = DimDescriptor {
            lower_bound: src.dims[i].lower_bound,
            upper_bound: src.dims[i].upper_bound,
            stride: 1,
        };
    }
    let dims_ptr = if src.rank > 0 {
        dims.as_ptr()
    } else {
        ptr::null()
    };
    afs_allocate_array(result, lane as i64, src.rank, dims_ptr, ptr::null_mut());

    let res = unsafe { &mut *result };
    let total = src.total_elements() as usize;
    let sp_buf = src.base_addr as *const u8;
    let rp_buf = res.base_addr;
    if elem_size == 8 {
        for i in 0..total {
            unsafe {
                let im = *(sp_buf.add(i * 8 + 4) as *const f32);
                *(rp_buf.add(i * 4) as *mut f32) = im;
            }
        }
    } else if elem_size == 16 {
        for i in 0..total {
            unsafe {
                let im = *(sp_buf.add(i * 16 + 8) as *const f64);
                *(rp_buf.add(i * 8) as *mut f64) = im;
            }
        }
    }
}

/// ABS over a complex array: produce a real array of the same shape
/// whose elements are |z| = sqrt(re*re + im*im). Result has HALF the
/// source elem_size; mirror the allocation strategy from `afs_array_aimag`.
#[no_mangle]
pub extern "C" fn afs_array_abs_complex(
    source: *const ArrayDescriptor,
    result: *mut ArrayDescriptor,
) {
    if source.is_null() || result.is_null() {
        return;
    }
    let src = unsafe { &*source };
    if src.base_addr.is_null() {
        return;
    }
    let elem_size = src.elem_size.max(1) as usize;
    let lane = elem_size / 2;
    let mut dims = [DimDescriptor::default(); MAX_RANK];
    for (i, dim) in dims.iter_mut().enumerate().take(src.rank as usize) {
        *dim = DimDescriptor {
            lower_bound: src.dims[i].lower_bound,
            upper_bound: src.dims[i].upper_bound,
            stride: 1,
        };
    }
    let dims_ptr = if src.rank > 0 {
        dims.as_ptr()
    } else {
        ptr::null()
    };
    afs_allocate_array(result, lane as i64, src.rank, dims_ptr, ptr::null_mut());

    let res = unsafe { &mut *result };
    let total = src.total_elements() as usize;
    let sp_buf = src.base_addr as *const u8;
    let rp_buf = res.base_addr;
    if elem_size == 8 {
        for i in 0..total {
            unsafe {
                let re = *(sp_buf.add(i * 8) as *const f32);
                let im = *(sp_buf.add(i * 8 + 4) as *const f32);
                *(rp_buf.add(i * 4) as *mut f32) = (re * re + im * im).sqrt();
            }
        }
    } else if elem_size == 16 {
        for i in 0..total {
            unsafe {
                let re = *(sp_buf.add(i * 16) as *const f64);
                let im = *(sp_buf.add(i * 16 + 8) as *const f64);
                *(rp_buf.add(i * 8) as *mut f64) = (re * re + im * im).sqrt();
            }
        }
    }
}

/// F2018 §16.9.43 CMPLX(re, im, kind) over real arrays.
///
/// Allocates a complex(out_lane_bytes) result of the same shape as `re_source`
/// and writes one element per source element with re[i] in lane 0 and
/// im[i] (or 0 when im_source is null) in lane 1. Handles cross-kind
/// inputs (real(sp) ↔ real(dp)) by reading the per-side elem_size.
///
/// `out_lane_bytes` is 4 (single) or 8 (double); result elem_size is
/// `2 * out_lane_bytes`. Kind tags match afs_assign_allocatable_convert:
/// 0=i8, 1=i16, 2=i32, 3=i64, 4=f32, 5=f64, 6=complex(f32), 7=complex(f64).
#[no_mangle]
pub extern "C" fn afs_array_cmplx(
    re_source: *const ArrayDescriptor,
    im_source: *const ArrayDescriptor,
    out_lane_bytes: i32,
    result: *mut ArrayDescriptor,
    re_kind_tag: i32,
    im_kind_tag: i32,
) {
    if re_source.is_null() || result.is_null() {
        return;
    }
    let re = unsafe { &*re_source };
    if re.base_addr.is_null() {
        return;
    }
    let im_opt = if im_source.is_null() {
        None
    } else {
        let im = unsafe { &*im_source };
        if im.base_addr.is_null() {
            None
        } else {
            Some(im)
        }
    };
    let lane = out_lane_bytes.max(4) as usize;
    let elem_size = 2 * lane;
    let mut dims = [DimDescriptor::default(); MAX_RANK];
    for (i, dim) in dims.iter_mut().enumerate().take(re.rank as usize) {
        *dim = DimDescriptor {
            lower_bound: re.dims[i].lower_bound,
            upper_bound: re.dims[i].upper_bound,
            stride: 1,
        };
    }
    let dims_ptr = if re.rank > 0 {
        dims.as_ptr()
    } else {
        ptr::null()
    };
    afs_allocate_array(result, elem_size as i64, re.rank, dims_ptr, ptr::null_mut());

    let res = unsafe { &mut *result };
    let total = re.total_elements() as usize;
    let kind_elem_size = |tag: i32, fallback: i64| -> usize {
        match tag {
            0 => 1,
            1 => 2,
            2 | 4 => 4,
            3 | 5 => 8,
            6 => 8,
            7 => 16,
            _ => fallback.max(1) as usize,
        }
    };
    let re_elem = kind_elem_size(re_kind_tag, re.elem_size);
    let im_elem = im_opt
        .map(|im| kind_elem_size(im_kind_tag, im.elem_size))
        .unwrap_or(0);
    let re_buf = re.base_addr as *const u8;
    let im_buf = im_opt
        .map(|im| im.base_addr as *const u8)
        .unwrap_or(ptr::null());
    let rp_buf = res.base_addr;
    let read_numeric_lane = |buf: *const u8, off: usize, tag: i32, elem: usize| -> f64 {
        unsafe {
            match tag {
                0 => *(buf.add(off) as *const i8) as f64,
                1 => *(buf.add(off) as *const i16) as f64,
                2 => *(buf.add(off) as *const i32) as f64,
                3 => *(buf.add(off) as *const i64) as f64,
                4 => *(buf.add(off) as *const f32) as f64,
                5 => *(buf.add(off) as *const f64),
                6 => *(buf.add(off) as *const f32) as f64,
                7 => *(buf.add(off) as *const f64),
                _ => match elem {
                    4 => *(buf.add(off) as *const f32) as f64,
                    8 => *(buf.add(off) as *const f64),
                    _ => 0.0,
                },
            }
        }
    };
    let read_complex_imag_lane = |buf: *const u8, off: usize, tag: i32| -> f64 {
        unsafe {
            match tag {
                6 => *(buf.add(off + 4) as *const f32) as f64,
                7 => *(buf.add(off + 8) as *const f64),
                _ => 0.0,
            }
        }
    };
    for i in 0..total {
        let dst_off = i * elem_size;
        unsafe {
            let re_off = i * re_elem;
            let r_val = read_numeric_lane(re_buf, re_off, re_kind_tag, re_elem);
            // Read imag lane (zero when source absent).
            let i_val: f64 = if im_buf.is_null() {
                read_complex_imag_lane(re_buf, re_off, re_kind_tag)
            } else {
                read_numeric_lane(im_buf, i * im_elem, im_kind_tag, im_elem)
            };
            // Write per result kind.
            match lane {
                4 => {
                    *(rp_buf.add(dst_off) as *mut f32) = r_val as f32;
                    *(rp_buf.add(dst_off + 4) as *mut f32) = i_val as f32;
                }
                8 => {
                    *(rp_buf.add(dst_off) as *mut f64) = r_val;
                    *(rp_buf.add(dst_off + 8) as *mut f64) = i_val;
                }
                _ => {}
            }
        }
    }
}

/// F2018 §16.9.144 PACK(ARRAY, MASK [, VECTOR]).
///
/// Walks `source` and `mask` element-by-element (mask is interpreted
/// element-wise, regardless of source rank, since shapes must conform
/// per the standard). Each source element whose mask element is true
/// is copied into a fresh rank-1 result descriptor.
///
/// `vector` is optional; when non-null, the result inherits its size
/// (element count) and elements past the masked-true count are filled
/// from `vector`. Otherwise the result size is the count of true
/// values in the mask.
///
/// `mask` is a Fortran logical, stored as i32 in our descriptor: zero
/// means false, anything else means true.
///
/// The element copy is byte-level via `elem_size` so this works for
/// any non-derived element type (integer/real/complex/logical/character
/// of any kind).
#[no_mangle]
pub extern "C" fn afs_array_pack(
    source: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
    vector: *const ArrayDescriptor,
    result: *mut ArrayDescriptor,
) {
    if source.is_null() || mask.is_null() || result.is_null() {
        return;
    }
    let src = unsafe { &*source };
    let msk = unsafe { &*mask };
    let elem_size = src.elem_size.max(1) as usize;
    let total = src.total_elements() as usize;
    let mask_total = msk.total_elements() as usize;
    let pairs = if msk.rank == 0 {
        total
    } else {
        total.min(mask_total)
    };
    if pairs > 0 && (src.base_addr.is_null() || msk.base_addr.is_null()) {
        return;
    }

    // First pass: count true values in the mask. Dispatch on the
    // mask's elem_size — a `logical :: m(:)` now reaches us with
    // elem_size=1 (matches storage), and the prior fixed i32 read
    // misaligned every iteration.
    let mut true_count: i64 = 0;
    for i in 0..pairs {
        let mask_off = if msk.rank == 0 {
            0
        } else {
            descriptor_linear_byte_offset(msk, i)
        };
        if unsafe { mask_byte_is_true(msk, mask_off) } {
            true_count += 1;
        }
    }

    // Result size: vector's size if provided, else count of trues.
    let result_n = if !vector.is_null() {
        let vec = unsafe { &*vector };
        vec.total_elements()
    } else {
        true_count
    };

    // Allocate rank-1 result descriptor.
    let dim = DimDescriptor {
        lower_bound: 1,
        upper_bound: result_n,
        stride: 1,
    };
    let dim_ptr = &dim as *const DimDescriptor;
    afs_allocate_array(result, elem_size as i64, 1, dim_ptr, ptr::null_mut());

    let res = unsafe { &mut *result };
    let sp = src.base_addr as *const u8;
    let rp = res.base_addr;

    // Second pass: emit masked-true source elements into result.
    let mut out_idx: usize = 0;
    for i in 0..pairs {
        let mask_off = if msk.rank == 0 {
            0
        } else {
            descriptor_linear_byte_offset(msk, i)
        };
        if unsafe { mask_byte_is_true(msk, mask_off) } {
            let src_off = descriptor_linear_byte_offset(src, i);
            unsafe {
                core::ptr::copy_nonoverlapping(
                    sp.add(src_off),
                    rp.add(out_idx * elem_size),
                    elem_size,
                );
            }
            out_idx += 1;
        }
    }

    // Pad the tail from `vector` (if provided and result_n > true_count).
    if !vector.is_null() {
        let vec = unsafe { &*vector };
        if !vec.base_addr.is_null() {
            let vp = vec.base_addr as *const u8;
            let tail_start = out_idx;
            let tail_end = result_n as usize;
            for j in tail_start..tail_end {
                let vec_off = descriptor_linear_byte_offset(vec, j);
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        vp.add(vec_off),
                        rp.add(j * elem_size),
                        elem_size,
                    );
                }
            }
        }
    }
}

/// F2018 §16.9.194 UNPACK(VECTOR, MASK, FIELD).
///
/// Allocates `result` with MASK's shape and VECTOR's element size.
/// Elements whose mask is true are copied from VECTOR in order; false
/// elements are copied from FIELD at the same flat array position.
#[no_mangle]
pub extern "C" fn afs_array_unpack(
    vector: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
    field: *const ArrayDescriptor,
    result: *mut ArrayDescriptor,
) {
    if vector.is_null() || mask.is_null() || field.is_null() || result.is_null() {
        return;
    }
    let vec = unsafe { &*vector };
    let msk = unsafe { &*mask };
    let fld = unsafe { &*field };
    if vec.base_addr.is_null() || msk.base_addr.is_null() || fld.base_addr.is_null() {
        return;
    }

    let elem_size = vec.elem_size.max(1) as usize;
    let mut stat = 0i32;
    afs_allocate_like_with_elem_size(result, mask, elem_size as i64, &mut stat as *mut i32);
    if stat != 0 {
        return;
    }
    let res = unsafe { &mut *result };
    if res.base_addr.is_null() {
        return;
    }

    let total = msk.total_elements().max(0) as usize;
    let vec_total = vec.total_elements().max(0) as usize;
    let field_total = fld.total_elements().max(0) as usize;
    let mask_elem = msk.elem_size.max(1) as usize;
    let field_elem = fld.elem_size.max(1) as usize;
    let vp = vec.base_addr as *const u8;
    let fp = fld.base_addr as *const u8;
    let rp = res.base_addr;
    let mut vec_idx = 0usize;

    for i in 0..total {
        let dest = unsafe { rp.add(i * elem_size) };
        let take_vector = unsafe { mask_byte_is_true(msk, i * mask_elem) };
        if take_vector {
            if vec_idx < vec_total {
                unsafe {
                    core::ptr::copy_nonoverlapping(vp.add(vec_idx * elem_size), dest, elem_size);
                }
            } else {
                unsafe {
                    core::ptr::write_bytes(dest, 0, elem_size);
                }
            }
            vec_idx += 1;
        } else if i < field_total {
            unsafe {
                core::ptr::copy_nonoverlapping(fp.add(i * field_elem), dest, elem_size);
            }
        } else {
            unsafe {
                core::ptr::write_bytes(dest, 0, elem_size);
            }
        }
    }
}

/// F2018 §16.9.163: RESHAPE(SOURCE, SHAPE [, PAD, ORDER]).
///
/// Allocates a fresh result descriptor of rank = size(shape) and
/// element-fills it from `source` in array-element order. When
/// `order` is supplied (a permutation of 1..rank), the *target*
/// dimension traversal is permuted: result element index `(j1,...,jN)`
/// corresponds to a logical "natural" position whose subscripts are
/// `(j[order(1)],...,j[order(N)])`. When the result has more elements
/// than the source, the tail is filled cyclically from `pad`.
///
/// Shape and order arrays are i32 or i64 — read both via the same
/// 64-bit-extended path keyed off the descriptor's elem_size.
#[no_mangle]
pub extern "C" fn afs_array_reshape(
    source: *const ArrayDescriptor,
    shape: *const ArrayDescriptor,
    order: *const ArrayDescriptor,
    pad: *const ArrayDescriptor,
    result: *mut ArrayDescriptor,
) {
    if source.is_null() || shape.is_null() || result.is_null() {
        return;
    }
    let src = unsafe { &*source };
    let shp = unsafe { &*shape };
    if src.base_addr.is_null() || shp.base_addr.is_null() {
        return;
    }
    let rank = shp.total_elements() as usize;
    if rank == 0 || rank > MAX_RANK {
        return;
    }

    // Read shape extents into a fixed-size array.
    let read_int_at = |buf: *const u8, idx: usize, elem_size: usize| -> i64 {
        unsafe {
            match elem_size {
                4 => *(buf.add(idx * 4) as *const i32) as i64,
                8 => *(buf.add(idx * 8) as *const i64),
                _ => 0,
            }
        }
    };
    let shape_buf = shp.base_addr as *const u8;
    let shape_elem = shp.elem_size.max(1) as usize;
    let mut extents: [i64; MAX_RANK] = [0; MAX_RANK];
    for (i, slot) in extents.iter_mut().enumerate().take(rank) {
        *slot = read_int_at(shape_buf, i, shape_elem).max(0);
    }

    // Build dim descriptors and allocate result.
    let mut dims = [DimDescriptor::default(); MAX_RANK];
    for i in 0..rank {
        dims[i] = DimDescriptor {
            lower_bound: 1,
            upper_bound: extents[i],
            stride: 1,
        };
    }
    let elem_size = src.elem_size.max(1);
    afs_allocate_array(
        result,
        elem_size,
        rank as i32,
        dims.as_ptr(),
        ptr::null_mut(),
    );
    let res = unsafe { &mut *result };
    if res.base_addr.is_null() {
        return;
    }

    let total: i64 = extents.iter().take(rank).copied().product();
    let total_usize = total as usize;
    let src_total = src.total_elements() as usize;
    let elem_size_usize = elem_size as usize;

    // Read order (identity if absent or invalid). ORDER must be a
    // permutation of 1..rank; never let malformed data index the fixed
    // descriptor arrays.
    let mut order_perm: [usize; MAX_RANK] = [0; MAX_RANK];
    for (i, slot) in order_perm.iter_mut().enumerate().take(rank) {
        *slot = i;
    }
    let order_present = !order.is_null() && unsafe { (*order).rank > 0 };
    if order_present {
        let ord = unsafe { &*order };
        if !ord.base_addr.is_null() {
            let ord_buf = ord.base_addr as *const u8;
            let ord_elem = ord.elem_size.max(1) as usize;
            let ord_count = ord.total_elements() as usize;
            let mut candidate: [usize; MAX_RANK] = [0; MAX_RANK];
            let mut seen: [bool; MAX_RANK] = [false; MAX_RANK];
            let mut valid = ord_count >= rank;
            if valid {
                for (i, slot) in candidate.iter_mut().enumerate().take(rank) {
                    let raw = read_int_at(ord_buf, i, ord_elem);
                    if raw < 1 || raw as usize > rank {
                        valid = false;
                        break;
                    }
                    let dim = (raw - 1) as usize;
                    if seen[dim] {
                        valid = false;
                        break;
                    }
                    seen[dim] = true;
                    *slot = dim;
                }
            }
            if valid {
                order_perm[..rank].copy_from_slice(&candidate[..rank]);
            }
        }
    }

    let pad_present = !pad.is_null() && unsafe { (*pad).total_elements() > 0 };
    let (pad_buf, pad_total) = if pad_present {
        let p = unsafe { &*pad };
        (p.base_addr as *const u8, p.total_elements() as usize)
    } else {
        (ptr::null(), 0)
    };

    let sp = src.base_addr as *const u8;
    let rp = res.base_addr;

    // Linear iteration over the result in element order. For each
    // result linear index, compute the multi-dim subscript in the
    // *natural* (un-permuted) order, then look up the target slot
    // by applying `order_perm` to translate logical → result subscript.
    for linear in 0..total_usize {
        // Natural multi-dim subscript: column-major over extents in
        // logical order, where logical extents follow the permutation
        // (logical_dim k = extents[order_perm[k]]).
        let mut idx = linear;
        let mut logical_subs: [i64; MAX_RANK] = [0; MAX_RANK];
        for k in 0..rank {
            let logical_extent = extents[order_perm[k]].max(1) as usize;
            logical_subs[k] = (idx % logical_extent) as i64;
            idx /= logical_extent;
        }
        // Translate into result subscript: result_subs[order_perm[k]] = logical_subs[k]
        let mut result_subs: [i64; MAX_RANK] = [0; MAX_RANK];
        for k in 0..rank {
            result_subs[order_perm[k]] = logical_subs[k];
        }
        // Compute result linear (column-major over result extents).
        let mut result_linear: usize = 0;
        let mut multiplier: usize = 1;
        for k in 0..rank {
            result_linear += (result_subs[k] as usize) * multiplier;
            multiplier *= extents[k].max(1) as usize;
        }
        // Source element: linear (column-major as if rank-1 flat).
        let src_off = if linear < src_total {
            linear * elem_size_usize
        } else if pad_total > 0 {
            ((linear - src_total) % pad_total) * elem_size_usize
        } else {
            0
        };
        let from = if linear < src_total || pad_total == 0 {
            sp
        } else {
            pad_buf
        };
        unsafe {
            core::ptr::copy_nonoverlapping(
                from.add(src_off),
                rp.add(result_linear * elem_size_usize),
                elem_size_usize,
            );
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

/// DOT_PRODUCT(a, b) — vector dot product (complex(real32) version).
/// Fortran conjugates the first complex vector argument.
#[no_mangle]
pub extern "C" fn afs_dot_product_complex4(
    a: *const ArrayDescriptor,
    b: *const ArrayDescriptor,
    out: *mut f32,
) {
    if out.is_null() {
        return;
    }
    unsafe {
        *out = 0.0;
        *out.add(1) = 0.0;
    }
    if a.is_null() || b.is_null() {
        return;
    }
    let da = unsafe { &*a };
    let db = unsafe { &*b };
    if da.base_addr.is_null() || db.base_addr.is_null() {
        return;
    }
    let n = da.dims[0].extent().min(db.dims[0].extent()) as usize;
    let stride_a = da.dims[0].stride.max(1) as usize;
    let stride_b = db.dims[0].stride.max(1) as usize;
    let elem_a = da.elem_size.max(8) as usize;
    let elem_b = db.elem_size.max(8) as usize;
    let mut re = 0.0f32;
    let mut im = 0.0f32;
    for i in 0..n {
        let pa = unsafe { da.base_addr.add(i * stride_a * elem_a) as *const f32 };
        let pb = unsafe { db.base_addr.add(i * stride_b * elem_b) as *const f32 };
        let ar = unsafe { *pa };
        let ai = unsafe { *pa.add(1) };
        let br = unsafe { *pb };
        let bi = unsafe { *pb.add(1) };
        re += ar * br + ai * bi;
        im += ar * bi - ai * br;
    }
    unsafe {
        *out = re;
        *out.add(1) = im;
    }
}

/// DOT_PRODUCT(a, b) — vector dot product (complex(real64) version).
/// Fortran conjugates the first complex vector argument.
#[no_mangle]
pub extern "C" fn afs_dot_product_complex8(
    a: *const ArrayDescriptor,
    b: *const ArrayDescriptor,
    out: *mut f64,
) {
    if out.is_null() {
        return;
    }
    unsafe {
        *out = 0.0;
        *out.add(1) = 0.0;
    }
    if a.is_null() || b.is_null() {
        return;
    }
    let da = unsafe { &*a };
    let db = unsafe { &*b };
    if da.base_addr.is_null() || db.base_addr.is_null() {
        return;
    }
    let n = da.dims[0].extent().min(db.dims[0].extent()) as usize;
    let stride_a = da.dims[0].stride.max(1) as usize;
    let stride_b = db.dims[0].stride.max(1) as usize;
    let elem_a = da.elem_size.max(16) as usize;
    let elem_b = db.elem_size.max(16) as usize;
    let mut re = 0.0f64;
    let mut im = 0.0f64;
    for i in 0..n {
        let pa = unsafe { da.base_addr.add(i * stride_a * elem_a) as *const f64 };
        let pb = unsafe { db.base_addr.add(i * stride_b * elem_b) as *const f64 };
        let ar = unsafe { *pa };
        let ai = unsafe { *pa.add(1) };
        let br = unsafe { *pb };
        let bi = unsafe { *pb.add(1) };
        re += ar * br + ai * bi;
        im += ar * bi - ai * br;
    }
    unsafe {
        *out = re;
        *out.add(1) = im;
    }
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
