//! Array and string descriptors — the ABI contract between codegen and runtime.
//!
//! These repr(C) structs define the binary layout that generated ARM64 code
//! reads/writes directly. The runtime functions operate on pointers to these
//! descriptors. **This layout is stable once committed** — changing it requires
//! recompiling all Fortran code.
//!
//! Design choices:
//! - Max rank 15 (Fortran standard allows up to 15 dimensions)
//! - Stride in elements (not bytes) — multiply by elem_size for byte offset
//! - Flags bitfield for allocated/contiguous/pointer status
//! - Fixed-size dims array (no heap allocation for the descriptor itself)

use std::ptr;

/// Maximum array rank (Fortran 2018 allows up to 15).
pub const MAX_RANK: usize = 15;

/// Descriptor flags.
pub const DESC_ALLOCATED: u32 = 1 << 0;
pub const DESC_CONTIGUOUS: u32 = 1 << 1;
pub const DESC_POINTER: u32 = 1 << 2;
/// Compiler-private array CLASS(*) dynamic type bits stored above flags.
pub const DESC_TYPE_TAG_MASK: u32 = 0xffff_ff00;

/// Array descriptor — the runtime representation of a Fortran array.
///
/// Layout:
/// ```text
/// offset  field          size
/// 0       base_addr      8 bytes (pointer)
/// 8       elem_size      8 bytes (i64)
/// 16      rank           4 bytes (i32)
/// 20      flags          4 bytes (u32; low 8 status bits, high 24 type tag)
/// 24      dims[0]        24 bytes (DimDescriptor)
/// 48      dims[1]        24 bytes
/// ...
/// 384     dims[14]       24 bytes
/// total: 384 bytes
/// ```
#[repr(C)]
#[derive(Clone)]
pub struct ArrayDescriptor {
    /// Pointer to the first element of the array data.
    pub base_addr: *mut u8,
    /// Size of one element in bytes.
    pub elem_size: i64,
    /// Number of dimensions (1-15).
    pub rank: i32,
    /// Status flags: allocated, contiguous, pointer.
    pub flags: u32,
    /// Per-dimension information (lower bound, upper bound, stride).
    pub dims: [DimDescriptor; MAX_RANK],
}

/// Per-dimension descriptor.
#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct DimDescriptor {
    /// Lower bound (inclusive). Default is 1 per Fortran convention.
    pub lower_bound: i64,
    /// Upper bound (inclusive).
    pub upper_bound: i64,
    /// Stride in elements (not bytes). 1 for contiguous, >1 for sections.
    pub stride: i64,
}

impl DimDescriptor {
    /// Number of elements along this dimension.
    ///
    /// Throughout this codebase a `DimDescriptor`'s `(lower_bound,
    /// upper_bound)` are the section's logical 1-based bounds and the
    /// `stride` field records the *memory* step (in elements) between
    /// adjacent logical positions — see `afs_create_section` and the
    /// matching IR loop body in `lower_array_assign`. The IR-level
    /// extent computation at lower.rs:28443 already uses
    /// `upper - lower + 1` and ignores stride; this Rust-side formula
    /// is the runtime mirror, so it must do the same.
    pub fn extent(&self) -> i64 {
        if self.upper_bound < self.lower_bound {
            0
        } else {
            self.upper_bound - self.lower_bound + 1
        }
    }
}

impl ArrayDescriptor {
    /// Create a zeroed (unallocated) descriptor.
    pub fn zeroed() -> Self {
        Self {
            base_addr: ptr::null_mut(),
            elem_size: 0,
            rank: 0,
            flags: 0,
            dims: [DimDescriptor::default(); MAX_RANK],
        }
    }

    /// Check if the array is currently allocated.
    pub fn is_allocated(&self) -> bool {
        self.flags & DESC_ALLOCATED != 0
    }

    /// Check if the array is contiguous in memory.
    pub fn is_contiguous(&self) -> bool {
        self.flags & DESC_CONTIGUOUS != 0
    }

    /// Total number of elements across all dimensions.
    pub fn total_elements(&self) -> i64 {
        let mut total: i64 = 1;
        for i in 0..self.rank as usize {
            total *= self.dims[i].extent();
        }
        total
    }

    /// Total size in bytes: total_elements * elem_size.
    pub fn total_bytes(&self) -> i64 {
        self.total_elements() * self.elem_size
    }

    /// Compute the byte offset for an element given subscripts (0-based indices).
    /// Column-major order: first index varies fastest.
    pub fn element_offset(&self, subscripts: &[i64]) -> i64 {
        let mut offset: i64 = 0;
        let mut multiplier: i64 = 1;
        for i in 0..self.rank as usize {
            let idx = if i < subscripts.len() {
                subscripts[i] - self.dims[i].lower_bound
            } else {
                0
            };
            offset += idx * multiplier * self.dims[i].stride;
            multiplier *= self.dims[i].extent();
        }
        offset * self.elem_size
    }

    /// Set dimensions from a slice of (lower, upper) bounds.
    /// Assumes contiguous layout with stride=1 for each dimension.
    pub fn set_bounds(&mut self, bounds: &[(i64, i64)]) {
        self.rank = bounds.len() as i32;
        for (i, &(lo, hi)) in bounds.iter().enumerate() {
            self.dims[i] = DimDescriptor {
                lower_bound: lo,
                upper_bound: hi,
                stride: 1,
            };
        }
        self.flags |= DESC_CONTIGUOUS;
    }

    /// Runtime concrete-type tag for scalar polymorphic allocatables.
    ///
    /// The current ABI stores this in `dims[0].lower_bound` when `rank == 0`.
    pub fn scalar_type_tag(&self) -> i64 {
        if self.rank == 0 {
            self.dims[0].lower_bound
        } else {
            0
        }
    }

    pub fn set_scalar_type_tag(&mut self, tag: i64) {
        if self.rank == 0 {
            self.dims[0].lower_bound = tag;
            self.dims[0].stride = 0;
        }
    }

    pub fn scalar_tbp_lookup_ptr(&self) -> *mut u8 {
        if self.rank == 0 {
            self.dims[0].upper_bound as usize as *mut u8
        } else {
            ptr::null_mut()
        }
    }

    pub fn set_scalar_tbp_lookup_ptr(&mut self, ptr: *mut u8) {
        if self.rank == 0 {
            self.dims[0].upper_bound = ptr as usize as i64;
            self.dims[0].stride = 0;
        }
    }

    pub fn clear_scalar_type_tag(&mut self) {
        if self.rank == 0 {
            self.dims[0] = DimDescriptor::default();
        }
    }
}

/// String descriptor — the runtime representation of a Fortran character variable.
///
/// Layout:
/// ```text
/// offset  field      size
/// 0       data       8 bytes (pointer)
/// 8       len        8 bytes (i64, length in characters)
/// 16      capacity   8 bytes (i64, allocated bytes, for deferred-length)
/// 24      flags      4 bytes (u32)
/// total: 32 bytes (padded to 32 for alignment)
/// ```
#[repr(C)]
#[derive(Clone)]
pub struct StringDescriptor {
    /// Pointer to the character data (not null-terminated).
    pub data: *mut u8,
    /// Current length in characters (= bytes for kind=1).
    pub len: i64,
    /// Allocated capacity in bytes. For fixed-length strings, capacity == len.
    /// For deferred-length (allocatable), capacity >= len.
    pub capacity: i64,
    /// Flags: allocated, deferred-length, etc.
    pub flags: u32,
}

/// String descriptor flags.
pub const STR_ALLOCATED: u32 = 1 << 0;
pub const STR_DEFERRED: u32 = 1 << 1;

impl StringDescriptor {
    /// Create a zeroed (empty) string descriptor.
    pub fn zeroed() -> Self {
        Self {
            data: ptr::null_mut(),
            len: 0,
            capacity: 0,
            flags: 0,
        }
    }

    /// Create a descriptor for a fixed-length string (stack-allocated data).
    pub fn fixed(data: *mut u8, len: i64) -> Self {
        Self {
            data,
            len,
            capacity: len,
            flags: 0,
        }
    }

    /// Check if the string is allocated (deferred-length).
    pub fn is_allocated(&self) -> bool {
        self.flags & STR_ALLOCATED != 0
    }

    /// Get the string data as a byte slice.
    pub unsafe fn as_bytes(&self) -> &[u8] {
        if self.data.is_null() || self.len <= 0 {
            &[]
        } else {
            std::slice::from_raw_parts(self.data, self.len as usize)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn descriptor_zeroed() {
        let d = ArrayDescriptor::zeroed();
        assert!(!d.is_allocated());
        assert_eq!(d.rank, 0);
        assert!(d.base_addr.is_null());
    }

    #[test]
    fn dim_extent() {
        let dim = DimDescriptor {
            lower_bound: 1,
            upper_bound: 10,
            stride: 1,
        };
        assert_eq!(dim.extent(), 10);

        // Memory-stride convention: bounds are the section's logical
        // 1-based positions and stride is the inter-element memory
        // step. extent is `upper - lower + 1`, independent of stride.
        let dim2 = DimDescriptor {
            lower_bound: 1,
            upper_bound: 5,
            stride: 2,
        };
        assert_eq!(dim2.extent(), 5);

        let dim3 = DimDescriptor {
            lower_bound: 5,
            upper_bound: 3,
            stride: 1,
        };
        assert_eq!(dim3.extent(), 0); // empty
    }

    #[test]
    fn total_elements() {
        let mut d = ArrayDescriptor::zeroed();
        d.set_bounds(&[(1, 10), (1, 20)]);
        assert_eq!(d.total_elements(), 200);
        assert_eq!(d.rank, 2);
        assert!(d.is_contiguous());
    }

    #[test]
    fn element_offset_1d() {
        let mut d = ArrayDescriptor::zeroed();
        d.elem_size = 4; // i32
        d.set_bounds(&[(1, 10)]);
        // a(1) → offset 0, a(2) → offset 4, a(10) → offset 36
        assert_eq!(d.element_offset(&[1]), 0);
        assert_eq!(d.element_offset(&[2]), 4);
        assert_eq!(d.element_offset(&[10]), 36);
    }

    #[test]
    fn element_offset_2d_column_major() {
        let mut d = ArrayDescriptor::zeroed();
        d.elem_size = 8; // f64
        d.set_bounds(&[(1, 3), (1, 4)]); // 3x4 matrix
                                         // Column-major: a(1,1)=0, a(2,1)=8, a(3,1)=16, a(1,2)=24
        assert_eq!(d.element_offset(&[1, 1]), 0);
        assert_eq!(d.element_offset(&[2, 1]), 8);
        assert_eq!(d.element_offset(&[3, 1]), 16);
        assert_eq!(d.element_offset(&[1, 2]), 24);
        assert_eq!(d.element_offset(&[3, 4]), 88); // last element
    }

    #[test]
    fn string_descriptor() {
        let s = StringDescriptor::zeroed();
        assert!(!s.is_allocated());
        assert_eq!(s.len, 0);
        assert!(s.data.is_null());
    }

    #[test]
    fn descriptor_size_is_stable() {
        // ABI stability: descriptor sizes must not change.
        assert_eq!(std::mem::size_of::<DimDescriptor>(), 24);
        assert_eq!(std::mem::size_of::<ArrayDescriptor>(), 384);
        assert_eq!(std::mem::size_of::<StringDescriptor>(), 32);
    }
}
