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
    desc.clear_scalar_type_tag();
    if !dims_ptr.is_null() && rank > 0 {
        let dims_slice = unsafe { std::slice::from_raw_parts(dims_ptr, rank as usize) };
        for (i, dim) in dims_slice.iter().enumerate() {
            desc.dims[i] = *dim;
        }
    }

    // ALLOCATE always produces a single contiguous block, so the
    // descriptor's per-dim memory stride must be the column-major
    // canonical step (1 for dim 0, then product of preceding extents).
    // Compiler-generated dim_buf entries pass stride=1 across the
    // board (the rank-1 case happens to be correct, multi-dim wasn't),
    // so fix it up here. Without this, allocatable rank-N section
    // assignments fed afs_create_section a stride=1 source and the
    // produced section descriptor stepped through memory contiguously,
    // collapsing column-major rows into a single linear walk and
    // corrupting both the LHS write and any subsequent RHS read.
    let mut running_stride: i64 = 1;
    for i in 0..rank as usize {
        desc.dims[i].stride = running_stride;
        running_stride = running_stride.saturating_mul(desc.dims[i].extent().max(1));
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
        if !stat.is_null() {
            unsafe {
                *stat = 1;
            }
        }
        return;
    }

    let source = unsafe { &*source };
    let mut dims = [DimDescriptor::default(); MAX_RANK];
    // Column-major running strides: dim[0]=1, dim[k]=Π_{j<k} extent_j.
    // The previous flat-1 stride caused per-dim consumers
    // (`for_each_reduce_along_dim`, `for_each_reduce_along_dim_with_mask`,
    // `lower_multi_d_section_assign`) to compute colliding byte offsets,
    // so for `m = (y > 3.)` over `real :: y(2,3)` only 4 of the 6 mask
    // bytes were read by the masked sum-along-dim helper — the
    // remaining two ran over the same byte twice and dropped the
    // column-2 hit entirely.
    let mut running: i64 = 1;
    for (i, dim) in dims.iter_mut().enumerate().take(source.rank as usize) {
        let extent = source.dims[i].extent();
        *dim = DimDescriptor {
            lower_bound: 1,
            upper_bound: extent,
            stride: running,
        };
        running = running.saturating_mul(extent.max(1));
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
    let dest = unsafe { &mut *dest };
    dest.set_scalar_type_tag(source.scalar_type_tag());
    dest.set_scalar_tbp_lookup_ptr(source.scalar_tbp_lookup_ptr());
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
    dest.set_scalar_tbp_lookup_ptr(source.scalar_tbp_lookup_ptr());

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
    // owned.  Treat the source as valid as long as it has a non-null
    // base_addr; require DESC_ALLOCATED only on the freshly-allocated
    // destination.
    let ok = dest.is_allocated()
        && !source.base_addr.is_null()
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
    desc.clear_scalar_type_tag();
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
    // dim's stride is *strictly greater* than its canonical
    // column-major step. Strides smaller than canonical (e.g.
    // afs_matmul's 2x2 result emitted with stride=(1,1) instead of
    // (1,2)) describe an internally inconsistent descriptor whose
    // base_addr still points at a flat contiguous buffer; walking
    // those would re-read the same byte offset twice and drop the
    // last element. The conservative choice is the flat copy that
    // mirrors total_bytes — which the previous unconditional ptr::copy
    // did silently for both kinds of source.
    let bytes = source.total_bytes();
    if bytes > 0 && !source.base_addr.is_null() && !dest.base_addr.is_null() {
        let elem_size = source.elem_size;
        let mut canonical: i64 = 1;
        let mut strided = false;
        for i in 0..source.rank as usize {
            if source.dims[i].stride > canonical {
                strided = true;
                break;
            }
            canonical = canonical.saturating_mul(source.dims[i].extent().max(1));
        }
        if !strided {
            unsafe {
                ptr::copy(source.base_addr, dest.base_addr, bytes as usize);
            }
        } else {
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
    dest.set_scalar_tbp_lookup_ptr(source.scalar_tbp_lookup_ptr());
}

/// Element-converting allocatable assignment.
///
/// F2018 §10.2.1.3: when the LHS and RHS of an array assignment have
/// different numeric element types, each element is converted to the
/// LHS type. This entry point performs that conversion when the source
/// descriptor's element kind differs from the destination's.
///
/// kind_tag: 0=i8, 1=i16, 2=i32, 3=i64, 4=f32, 5=f64
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

    if !source_ref.is_allocated() && source_ref.base_addr.is_null() {
        if dest_ref.is_allocated() && !dest_ref.base_addr.is_null() {
            unsafe {
                libc_free(dest_ref.base_addr);
            }
        }
        *dest_ref = ArrayDescriptor::zeroed();
        return;
    }

    let dest_elem_size: i64 = match dest_kind_tag {
        0 => 1,
        1 => 2,
        2 | 4 => 4,
        3 | 5 => 8,
        6 => 8,
        7 => 16,
        _ => return,
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
    let src_elem_size: i64 = match src_kind_tag {
        0 => 1,
        1 => 2,
        2 | 4 => 4,
        3 | 5 => 8,
        6 => 8,
        7 => 16,
        _ => return,
    };
    // Source may be non-contiguous (e.g. transpose result, section).
    // Walk each multi-index column-major and apply per-dim strides.
    // Mirror the same-class detection used in afs_assign_allocatable
    // and afs_copy_array_data: only apply per-dim strides when at
    // least one stride is *strictly greater* than its canonical
    // column-major step. A stride below canonical describes a
    // malformed descriptor (e.g. a 2x2 matmul result with stride=(1,1)
    // instead of (1,2)) whose underlying buffer is still flat
    // contiguous; walking those re-reads the same offset twice.
    let rank = source_ref.rank as usize;
    let extents: Vec<i64> = (0..rank).map(|i| source_ref.dims[i].extent()).collect();
    let raw_strides: Vec<i64> = (0..rank).map(|i| source_ref.dims[i].stride).collect();
    let mut canonical_step: i64 = 1;
    let mut canonical: Vec<i64> = Vec::with_capacity(rank);
    let mut strided = false;
    for d in 0..rank {
        canonical.push(canonical_step);
        if raw_strides[d] > canonical_step {
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
    from_desc.clear_scalar_type_tag();
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
    result.flags = DESC_CONTIGUOUS; // sections may not be contiguous
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

fn logical_desc_value(desc: &ArrayDescriptor, index: usize) -> bool {
    if desc.base_addr.is_null() || desc.rank < 1 {
        return false;
    }
    let stride = desc.dims[0].stride.max(1) as usize;
    let elem_size = desc.elem_size.max(1) as usize;
    let byte_offset = index.saturating_mul(stride).saturating_mul(elem_size);
    let ptr = unsafe { desc.base_addr.add(byte_offset) };
    unsafe { *ptr != 0 }
}

/// ANY(array) for logical arrays — return 1 when any element is true.
#[no_mangle]
pub extern "C" fn afs_array_any_logical(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() {
        return 0;
    }
    let d = unsafe { &*desc };
    let n = d.total_elements().max(0) as usize;
    for i in 0..n {
        if logical_desc_value(d, i) {
            return 1;
        }
    }
    0
}

/// ALL(array) for logical arrays — return 1 when every element is true.
#[no_mangle]
pub extern "C" fn afs_array_all_logical(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() {
        return 0;
    }
    let d = unsafe { &*desc };
    let n = d.total_elements().max(0) as usize;
    for i in 0..n {
        if !logical_desc_value(d, i) {
            return 0;
        }
    }
    1
}

/// COUNT(array) for logical arrays — number of true elements.
#[no_mangle]
pub extern "C" fn afs_array_count_logical(desc: *const ArrayDescriptor) -> i32 {
    if desc.is_null() {
        return 0;
    }
    let d = unsafe { &*desc };
    let n = d.total_elements().max(0) as usize;
    let mut count = 0i32;
    for i in 0..n {
        if logical_desc_value(d, i) {
            count += 1;
        }
    }
    count
}

/// COUNT(mask, DIM=k) — reduce along dimension k, allocate `dst` with
/// rank `mask.rank - 1` and extents = mask extents minus the reduction
/// dim, fill with per-slice counts of true elements (i32). Caller passes
/// a zeroed 384-byte descriptor; this helper populates it. Surfaced in
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
    if s.base_addr.is_null() {
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
    let n = d.total_elements() as usize;
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const f64;
    let mut acc = 0.0_f64;
    for i in 0..n {
        let v = unsafe { *ptr.add(i * stride) };
        acc += v * v;
    }
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
    let n = d.total_elements() as usize;
    let stride = d.dims[0].stride.max(1) as usize;
    let ptr = d.base_addr as *const f32;
    let mut acc = 0.0_f64;
    for i in 0..n {
        let v = unsafe { *ptr.add(i * stride) } as f64;
        acc += v * v;
    }
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
    if s.base_addr.is_null() || dim as usize > s.rank as usize {
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
    if s.base_addr.is_null() || dim as usize > s.rank as usize {
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
    let n = d.total_elements() as usize;
    let stride = d.dims[0].stride.max(1) as usize;
    let mut sum: f64 = 0.0;
    if d.elem_size == 4 {
        let ptr = d.base_addr as *const f32;
        for i in 0..n {
            sum += unsafe { *ptr.add(i * stride) } as f64;
        }
    } else {
        let ptr = d.base_addr as *const f64;
        for i in 0..n {
            sum += unsafe { *ptr.add(i * stride) };
        }
    }
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

/// SUM(array, DIM=k) — reduce along dimension k, allocate `dst` with
/// rank `src.rank - 1` and extents = src extents minus the reduction
/// dim, then write the per-slice sums into dst. Caller passes a
/// zeroed 384-byte descriptor; this helper populates rank/dims/flags
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
    if s.base_addr.is_null() {
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
    if s.base_addr.is_null() {
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
    if s.base_addr.is_null() {
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
    if s.base_addr.is_null() {
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
    if s.base_addr.is_null() || m.base_addr.is_null() {
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
    if s.base_addr.is_null() || m.base_addr.is_null() {
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
    if s.base_addr.is_null() || m.base_addr.is_null() {
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
    if s.base_addr.is_null() || m.base_addr.is_null() {
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
    let n = d.total_elements() as usize;
    let stride = d.dims[0].stride.max(1) as usize;
    let mut sum: i64 = 0;
    match d.elem_size {
        1 => {
            let ptr = d.base_addr as *const i8;
            for i in 0..n {
                sum = sum.wrapping_add(unsafe { *ptr.add(i * stride) } as i64);
            }
        }
        2 => {
            let ptr = d.base_addr as *const i16;
            for i in 0..n {
                sum = sum.wrapping_add(unsafe { *ptr.add(i * stride) } as i64);
            }
        }
        8 => {
            let ptr = d.base_addr as *const i64;
            for i in 0..n {
                sum = sum.wrapping_add(unsafe { *ptr.add(i * stride) });
            }
        }
        _ => {
            let ptr = d.base_addr as *const i32;
            for i in 0..n {
                sum = sum.wrapping_add(unsafe { *ptr.add(i * stride) } as i64);
            }
        }
    }
    sum
}

fn for_each_element_byte_offset<F: FnMut(usize)>(desc: &ArrayDescriptor, mut f: F) {
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
        strides[i] = desc.dims[i].stride.max(1);
        total *= extents[i];
    }

    let mut idx: [i64; 15] = [0; 15];
    for _ in 0..total {
        let mut byte_off = 0i64;
        for d in 0..rank {
            byte_off += idx[d] * strides[d] * desc.elem_size;
        }
        f(byte_off as usize);

        for d in 0..rank {
            idx[d] += 1;
            if idx[d] < extents[d] {
                break;
            }
            idx[d] = 0;
        }
    }
}

fn for_each_element_byte_offset_with_mask<F: FnMut(usize)>(
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
        s_strides[i] = desc.dims[i].stride.max(1);
        m_strides[i] = if (i as i32) < mask.rank {
            mask.dims[i].stride.max(1)
        } else {
            1
        };
        total *= extents[i];
    }

    let mut idx: [i64; 15] = [0; 15];
    let mask_elem = mask.elem_size.max(1);
    for _ in 0..total {
        let mut s_byte_off = 0i64;
        let mut m_byte_off = 0i64;
        for d in 0..rank {
            s_byte_off += idx[d] * s_strides[d] * desc.elem_size;
            m_byte_off += idx[d] * m_strides[d] * mask_elem;
        }
        if unsafe { mask_byte_is_true(mask, m_byte_off as usize) } {
            f(s_byte_off as usize);
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
        let p = src.add(byte_off) as *const f32;
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
        let p = src.add(byte_off) as *const f64;
        *out.add(0) += *p.add(0);
        *out.add(1) += *p.add(1);
    });
}

/// Read one logical element from `mask` at logical index `i` (zero-based)
/// honoring `mask.elem_size` (kind 1, 4, or any bit-width). Fortran maps
/// `.true.` to a non-zero stored value; we treat any non-zero byte as true.
unsafe fn mask_at(mask: &ArrayDescriptor, i: usize, stride: usize) -> bool {
    let off = i * stride * mask.elem_size.max(1) as usize;
    let p = mask.base_addr.add(off);
    let es = mask.elem_size as usize;
    match es {
        1 => *p != 0,
        2 => *(p as *const u16) != 0,
        4 => *(p as *const u32) != 0,
        8 => *(p as *const u64) != 0,
        _ => *p != 0,
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
    let n = d.total_elements() as usize;
    let stride_d = d.dims[0].stride.max(1) as usize;
    let stride_m = m.dims[0].stride.max(1) as usize;
    let mut sum: f64 = 0.0;
    if d.elem_size == 4 {
        let ptr = d.base_addr as *const f32;
        for i in 0..n {
            if unsafe { mask_at(m, i, stride_m) } {
                sum += unsafe { *ptr.add(i * stride_d) } as f64;
            }
        }
    } else {
        let ptr = d.base_addr as *const f64;
        for i in 0..n {
            if unsafe { mask_at(m, i, stride_m) } {
                sum += unsafe { *ptr.add(i * stride_d) };
            }
        }
    }
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
    let n = d.total_elements() as usize;
    let stride_d = d.dims[0].stride.max(1) as usize;
    let stride_m = mk.dims[0].stride.max(1) as usize;
    let mut sum: i64 = 0;
    macro_rules! sum_kind {
        ($t:ty) => {{
            let ptr = d.base_addr as *const $t;
            for i in 0..n {
                if unsafe { mask_at(mk, i, stride_m) } {
                    sum = sum.wrapping_add(unsafe { *ptr.add(i * stride_d) } as i64);
                }
            }
        }};
    }
    match d.elem_size {
        1 => sum_kind!(i8),
        2 => sum_kind!(i16),
        8 => sum_kind!(i64),
        _ => sum_kind!(i32),
    }
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
        let p = src.add(byte_off) as *const f32;
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
        let p = src.add(byte_off) as *const f64;
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
    let n = d.total_elements() as usize;
    let stride = d.dims[0].stride.max(1) as usize;
    let mut prod: f64 = 1.0;
    if d.elem_size == 4 {
        let ptr = d.base_addr as *const f32;
        for i in 0..n {
            prod *= unsafe { *ptr.add(i * stride) } as f64;
        }
    } else {
        let ptr = d.base_addr as *const f64;
        for i in 0..n {
            prod *= unsafe { *ptr.add(i * stride) };
        }
    }
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
    let n = d.total_elements() as usize;
    if n == 0 {
        return 1;
    }
    let stride = d.dims[0].stride.max(1) as usize;
    let mut prod: i64 = 1;
    match d.elem_size {
        1 => {
            let ptr = d.base_addr as *const i8;
            for i in 0..n {
                prod = prod.wrapping_mul(unsafe { *ptr.add(i * stride) } as i64);
            }
        }
        2 => {
            let ptr = d.base_addr as *const i16;
            for i in 0..n {
                prod = prod.wrapping_mul(unsafe { *ptr.add(i * stride) } as i64);
            }
        }
        8 => {
            let ptr = d.base_addr as *const i64;
            for i in 0..n {
                prod = prod.wrapping_mul(unsafe { *ptr.add(i * stride) });
            }
        }
        _ => {
            let ptr = d.base_addr as *const i32;
            for i in 0..n {
                prod = prod.wrapping_mul(unsafe { *ptr.add(i * stride) } as i64);
            }
        }
    }
    prod
}

/// PRODUCT(array, mask=mask) — masked product (real). Dispatches on
/// elem_size and reads mask via `mask_at` for any logical kind.
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
    let n = d.total_elements() as usize;
    let stride_d = d.dims[0].stride.max(1) as usize;
    let stride_m = m.dims[0].stride.max(1) as usize;
    let mut prod: f64 = 1.0;
    if d.elem_size == 4 {
        let ptr = d.base_addr as *const f32;
        for i in 0..n {
            if unsafe { mask_at(m, i, stride_m) } {
                prod *= unsafe { *ptr.add(i * stride_d) } as f64;
            }
        }
    } else {
        let ptr = d.base_addr as *const f64;
        for i in 0..n {
            if unsafe { mask_at(m, i, stride_m) } {
                prod *= unsafe { *ptr.add(i * stride_d) };
            }
        }
    }
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
    let n = d.total_elements() as usize;
    let stride_d = d.dims[0].stride.max(1) as usize;
    let stride_m = mk.dims[0].stride.max(1) as usize;
    let mut prod: i64 = 1;
    macro_rules! prod_kind {
        ($t:ty) => {{
            let ptr = d.base_addr as *const $t;
            for i in 0..n {
                if unsafe { mask_at(mk, i, stride_m) } {
                    prod = prod.wrapping_mul(unsafe { *ptr.add(i * stride_d) } as i64);
                }
            }
        }};
    }
    match d.elem_size {
        1 => prod_kind!(i8),
        2 => prod_kind!(i16),
        8 => prod_kind!(i64),
        _ => prod_kind!(i32),
    }
    prod
}

/// MAXVAL(array, mask=mask) — masked max (real). Returns -inf when no
/// element is selected.
#[no_mangle]
pub extern "C" fn afs_array_maxval_real8_mask(
    desc: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
) -> f64 {
    if desc.is_null() || mask.is_null() {
        return f64::NEG_INFINITY;
    }
    let d = unsafe { &*desc };
    let m = unsafe { &*mask };
    if d.base_addr.is_null() || m.base_addr.is_null() {
        return f64::NEG_INFINITY;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return f64::NEG_INFINITY;
    }
    let stride_d = d.dims[0].stride.max(1) as usize;
    let stride_m = m.dims[0].stride.max(1) as usize;
    let mut best = f64::NEG_INFINITY;
    if d.elem_size == 4 {
        let ptr = d.base_addr as *const f32;
        for i in 0..n {
            if unsafe { mask_at(m, i, stride_m) } {
                let v = unsafe { *ptr.add(i * stride_d) } as f64;
                if v > best {
                    best = v;
                }
            }
        }
    } else {
        let ptr = d.base_addr as *const f64;
        for i in 0..n {
            if unsafe { mask_at(m, i, stride_m) } {
                let v = unsafe { *ptr.add(i * stride_d) };
                if v > best {
                    best = v;
                }
            }
        }
    }
    best
}

/// MINVAL(array, mask=mask) — masked min (real).
#[no_mangle]
pub extern "C" fn afs_array_minval_real8_mask(
    desc: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
) -> f64 {
    if desc.is_null() || mask.is_null() {
        return f64::INFINITY;
    }
    let d = unsafe { &*desc };
    let m = unsafe { &*mask };
    if d.base_addr.is_null() || m.base_addr.is_null() {
        return f64::INFINITY;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return f64::INFINITY;
    }
    let stride_d = d.dims[0].stride.max(1) as usize;
    let stride_m = m.dims[0].stride.max(1) as usize;
    let mut best = f64::INFINITY;
    if d.elem_size == 4 {
        let ptr = d.base_addr as *const f32;
        for i in 0..n {
            if unsafe { mask_at(m, i, stride_m) } {
                let v = unsafe { *ptr.add(i * stride_d) } as f64;
                if v < best {
                    best = v;
                }
            }
        }
    } else {
        let ptr = d.base_addr as *const f64;
        for i in 0..n {
            if unsafe { mask_at(m, i, stride_m) } {
                let v = unsafe { *ptr.add(i * stride_d) };
                if v < best {
                    best = v;
                }
            }
        }
    }
    best
}

/// MAXVAL(array, mask=mask) — masked max (integer).
#[no_mangle]
pub extern "C" fn afs_array_maxval_int_mask(
    desc: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
) -> i64 {
    if desc.is_null() || mask.is_null() {
        return i64::MIN;
    }
    let d = unsafe { &*desc };
    let mk = unsafe { &*mask };
    if d.base_addr.is_null() || mk.base_addr.is_null() {
        return i64::MIN;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return i64::MIN;
    }
    let stride_d = d.dims[0].stride.max(1) as usize;
    let stride_m = mk.dims[0].stride.max(1) as usize;
    let mut best = i64::MIN;
    macro_rules! max_kind {
        ($t:ty) => {{
            let ptr = d.base_addr as *const $t;
            for i in 0..n {
                if unsafe { mask_at(mk, i, stride_m) } {
                    let v = unsafe { *ptr.add(i * stride_d) } as i64;
                    if v > best {
                        best = v;
                    }
                }
            }
        }};
    }
    match d.elem_size {
        1 => max_kind!(i8),
        2 => max_kind!(i16),
        8 => max_kind!(i64),
        _ => max_kind!(i32),
    }
    best
}

/// MINVAL(array, mask=mask) — masked min (integer).
#[no_mangle]
pub extern "C" fn afs_array_minval_int_mask(
    desc: *const ArrayDescriptor,
    mask: *const ArrayDescriptor,
) -> i64 {
    if desc.is_null() || mask.is_null() {
        return i64::MAX;
    }
    let d = unsafe { &*desc };
    let mk = unsafe { &*mask };
    if d.base_addr.is_null() || mk.base_addr.is_null() {
        return i64::MAX;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return i64::MAX;
    }
    let stride_d = d.dims[0].stride.max(1) as usize;
    let stride_m = mk.dims[0].stride.max(1) as usize;
    let mut best = i64::MAX;
    macro_rules! min_kind {
        ($t:ty) => {{
            let ptr = d.base_addr as *const $t;
            for i in 0..n {
                if unsafe { mask_at(mk, i, stride_m) } {
                    let v = unsafe { *ptr.add(i * stride_d) } as i64;
                    if v < best {
                        best = v;
                    }
                }
            }
        }};
    }
    match d.elem_size {
        1 => min_kind!(i8),
        2 => min_kind!(i16),
        8 => min_kind!(i64),
        _ => min_kind!(i32),
    }
    best
}

/// MAXVAL(array) — maximum element (real version). Dispatches on
/// `elem_size`; returns f64 for both real(4) and real(8).
/// Respects strides.
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
    if d.elem_size == 4 {
        let ptr = d.base_addr as *const f32;
        let mut max = unsafe { *ptr } as f64;
        for i in 1..n {
            let v = unsafe { *ptr.add(i * stride) } as f64;
            if v > max {
                max = v;
            }
        }
        max
    } else {
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
}

/// MINVAL(array) — minimum element (real version). Dispatches on
/// `elem_size`; returns f64 for both real(4) and real(8).
/// Respects strides.
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
    if d.elem_size == 4 {
        let ptr = d.base_addr as *const f32;
        let mut min = unsafe { *ptr } as f64;
        for i in 1..n {
            let v = unsafe { *ptr.add(i * stride) } as f64;
            if v < min {
                min = v;
            }
        }
        min
    } else {
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
    if d.base_addr.is_null() {
        return i64::MIN;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return i64::MIN;
    }
    let stride = d.dims[0].stride.max(1) as usize;
    match d.elem_size {
        1 => {
            let ptr = d.base_addr as *const i8;
            let mut max = unsafe { *ptr } as i64;
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) } as i64;
                if v > max {
                    max = v;
                }
            }
            max
        }
        2 => {
            let ptr = d.base_addr as *const i16;
            let mut max = unsafe { *ptr } as i64;
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) } as i64;
                if v > max {
                    max = v;
                }
            }
            max
        }
        8 => {
            let ptr = d.base_addr as *const i64;
            let mut max = unsafe { *ptr };
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) };
                if v > max {
                    max = v;
                }
            }
            max
        }
        _ => {
            let ptr = d.base_addr as *const i32;
            let mut max = unsafe { *ptr } as i64;
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) } as i64;
                if v > max {
                    max = v;
                }
            }
            max
        }
    }
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
    if d.base_addr.is_null() {
        return i64::MAX;
    }
    let n = d.total_elements() as usize;
    if n == 0 {
        return i64::MAX;
    }
    let stride = d.dims[0].stride.max(1) as usize;
    match d.elem_size {
        1 => {
            let ptr = d.base_addr as *const i8;
            let mut min = unsafe { *ptr } as i64;
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) } as i64;
                if v < min {
                    min = v;
                }
            }
            min
        }
        2 => {
            let ptr = d.base_addr as *const i16;
            let mut min = unsafe { *ptr } as i64;
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) } as i64;
                if v < min {
                    min = v;
                }
            }
            min
        }
        8 => {
            let ptr = d.base_addr as *const i64;
            let mut min = unsafe { *ptr };
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) };
                if v < min {
                    min = v;
                }
            }
            min
        }
        _ => {
            let ptr = d.base_addr as *const i32;
            let mut min = unsafe { *ptr } as i64;
            for i in 1..n {
                let v = unsafe { *ptr.add(i * stride) } as i64;
                if v < min {
                    min = v;
                }
            }
            min
        }
    }
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
    let elem_size = src.elem_size.max(1);

    // Allocate result as (n x m) using the source's element width so
    // real(4) and real(8) (and complex(4)/(8) when routed here) all
    // get the right buffer size and stride.
    afs_allocate_1d(result, elem_size, (n * m) as i64);
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
    let elem_size = da.elem_size.max(1);

    // Allocate result using the source element width so real(4) and
    // real(8) inputs both produce correctly-sized output buffers.
    afs_allocate_1d(result, elem_size, (m * n) as i64);
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
    let elem_size = da.elem_size.max(8);

    afs_allocate_1d(result, elem_size, (m * n) as i64);
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
    if src.rank < 2 || src.base_addr.is_null() {
        return;
    }

    let m = src.dims[0].extent() as usize;
    let n = src.dims[1].extent() as usize;
    let elem_size = src.elem_size.max(1) as usize;
    let sp = src.base_addr as *const u8;

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
    if src.base_addr.is_null() || msk.base_addr.is_null() {
        return;
    }
    let elem_size = src.elem_size.max(1) as usize;
    let total = src.total_elements() as usize;
    let mask_total = msk.total_elements() as usize;
    let pairs = total.min(mask_total);
    let mask_elem = msk.elem_size.max(1) as usize;

    // First pass: count true values in the mask. Dispatch on the
    // mask's elem_size — a `logical :: m(:)` now reaches us with
    // elem_size=1 (matches storage), and the prior fixed i32 read
    // misaligned every iteration.
    let mut true_count: i64 = 0;
    for i in 0..pairs {
        if unsafe { mask_byte_is_true(msk, i * mask_elem) } {
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
        if unsafe { mask_byte_is_true(msk, i * mask_elem) } {
            unsafe {
                core::ptr::copy_nonoverlapping(
                    sp.add(i * elem_size),
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
                unsafe {
                    core::ptr::copy_nonoverlapping(
                        vp.add(j * elem_size),
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

    // Read order (identity if absent).
    let mut order_perm: [usize; MAX_RANK] = [0; MAX_RANK];
    let order_present = !order.is_null() && unsafe { (*order).rank > 0 };
    if order_present {
        let ord = unsafe { &*order };
        let ord_buf = ord.base_addr as *const u8;
        let ord_elem = ord.elem_size.max(1) as usize;
        let ord_count = ord.total_elements() as usize;
        for (i, slot) in order_perm.iter_mut().enumerate().take(rank.min(ord_count)) {
            // Convert from 1-based Fortran to 0-based.
            *slot = (read_int_at(ord_buf, i, ord_elem) - 1).max(0) as usize;
        }
    } else {
        for (i, slot) in order_perm.iter_mut().enumerate().take(rank) {
            *slot = i;
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
