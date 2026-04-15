# Sprint 22: Runtime — Memory Management & Descriptors

## Prerequisites
Sprint 20 (calling convention — need to call runtime from generated code)

## Goals
Implement the runtime library's memory management: array descriptors, ALLOCATE/DEALLOCATE, automatic reallocation on assignment, and scope-exit cleanup. This is foundational — arrays and allocatables are everywhere in Fortran.

## Deliverables

### 1. Array Descriptor ABI
Define the binary layout that generated code and the runtime agree on:

```rust
#[repr(C)]
pub struct ArrayDescriptor {
    pub base_addr: *mut u8,
    pub elem_size: i64,
    pub rank: i32,
    pub flags: u32,              // bit flags: allocated, contiguous, etc.
    pub dims: [DimDescriptor; 15],  // max rank = 15
}

#[repr(C)]
pub struct DimDescriptor {
    pub lower_bound: i64,
    pub upper_bound: i64,
    pub stride: i64,             // in elements
}

// Flags
const DESC_ALLOCATED: u32 = 1;
const DESC_CONTIGUOUS: u32 = 2;
const DESC_POINTER: u32 = 4;
```

This descriptor is what gets passed when a subroutine takes an assumed-shape array argument. The codegen emits code that reads/writes these fields directly.

### 2. ALLOCATE Implementation
```rust
#[no_mangle]
pub extern "C" fn __afs_allocate(
    desc: *mut ArrayDescriptor,
    elem_size: i64,
    rank: i32,
    dims: *const DimDescriptor,
    stat: *mut i32,              // optional STAT variable (0 = success)
    errmsg: *mut StringDescriptor, // optional ERRMSG
) {
    // 1. Compute total element count from dims
    // 2. malloc(count * elem_size)
    // 3. Fill descriptor: base_addr, elem_size, rank, dims, flags
    // 4. Set STAT and ERRMSG if provided
}
```

### 3. DEALLOCATE Implementation
```rust
#[no_mangle]
pub extern "C" fn __afs_deallocate(
    desc: *mut ArrayDescriptor,
    stat: *mut i32,
    errmsg: *mut StringDescriptor,
) {
    // 1. Check if allocated (flag check)
    // 2. free(base_addr)
    // 3. Clear descriptor (base_addr = null, clear allocated flag)
    // 4. Set STAT and ERRMSG
}
```

### 4. Allocatable Assignment
Fortran 2003 allocatable assignment:
```fortran
real, allocatable :: a(:)
a = [1.0, 2.0, 3.0]    ! allocate a(3), copy values
a = [1.0, 2.0]          ! reallocate a(2), copy values
```

Runtime function:
```rust
#[no_mangle]
pub extern "C" fn __afs_assign_allocatable(
    dest: *mut ArrayDescriptor,
    source: *const ArrayDescriptor,
) {
    // 1. Check if dest shape matches source shape
    // 2. If not, deallocate dest, allocate with source's shape
    // 3. Copy data from source to dest
}
```

### 5. MOVE_ALLOC
```fortran
call move_alloc(from=temp, to=result)
! temp becomes unallocated, result takes temp's allocation
```

```rust
#[no_mangle]
pub extern "C" fn __afs_move_alloc(
    from: *mut ArrayDescriptor,
    to: *mut ArrayDescriptor,
) {
    // 1. Deallocate 'to' if allocated
    // 2. Copy descriptor from 'from' to 'to'
    // 3. Clear 'from' descriptor
}
```

### 6. Automatic Deallocation
Generated code must deallocate local allocatables at scope exit. The codegen inserts calls to `__afs_deallocate` on all exit paths:
- Normal function return
- RETURN statement
- EXIT from enclosing construct (if allocatable is local to that construct)

This is handled in codegen (Sprint 16 laid the groundwork), but the runtime provides the deallocation function.

### 7. Array Section Descriptors
When passing an array section to a subprogram:
```fortran
call process(matrix(:, j))     ! column j of matrix
call process(vector(1:n:2))    ! every other element
```

Create a new descriptor pointing into the original data with adjusted strides:
```rust
#[no_mangle]
pub extern "C" fn __afs_create_section(
    source: *const ArrayDescriptor,
    result: *mut ArrayDescriptor,
    dim_specs: *const SectionSpec,
    n_dims: i32,
) {
    // Compute new descriptor with:
    // - base_addr offset to section start
    // - strides adjusted for section step
    // - bounds matching section range
}
```

### 8. ALLOCATED Intrinsic
```rust
#[no_mangle]
pub extern "C" fn __afs_allocated(desc: *const ArrayDescriptor) -> i32 {
    unsafe { ((*desc).flags & DESC_ALLOCATED != 0) as i32 }
}
```

Also inlined by codegen for simple cases (just a flag check).

## Testing Strategy

### ALLOCATE/DEALLOCATE Cycle
```fortran
real, allocatable :: a(:)
allocate(a(100))
a = 1.0                    ! array assignment
print *, sum(a)            ! should print 100.0
deallocate(a)
print *, allocated(a)      ! should print F
```

### Reallocation Tests
```fortran
real, allocatable :: a(:)
a = [1.0, 2.0, 3.0]
print *, size(a)           ! 3
a = [4.0, 5.0]
print *, size(a)           ! 2 (reallocated)
```

### Memory Leak Tests
Compile programs that allocate/deallocate in loops. Run under memory sanitizer or custom tracking. Verify zero leaks.

### Array Section Tests
Pass array sections to subroutines, verify correct element access through section descriptor.

### Stress Tests
- Allocate large arrays (1M+ elements)
- Allocate and deallocate in tight loops (no leaks, no fragmentation issues)
- Deeply nested allocatable structures

## Key Technical Notes

### Descriptor Stability
The descriptor layout is our ABI. Once we commit to it, changing it requires recompiling everything. Get it right in this sprint.

### Stack vs Heap for Large Arrays
Arrays declared without ALLOCATABLE:
- Small (< 64KB): stack (alloca in IR)
- Large (>= 64KB): heap with automatic deallocation (this is where gfortran breaks!)

Our threshold is configurable but defaults to 64KB. This prevents the stack corruption that gfortran exhibits with 600KB+ arrays.

### Thread Safety
For now, the runtime is single-threaded. When we eventually support DO CONCURRENT parallelism, descriptor operations will need atomic flag updates. Design the data structures to accommodate this later.

## Definition of Done
- Array descriptors correctly represent rank 1-7 arrays
- ALLOCATE works with STAT and ERRMSG
- DEALLOCATE works with STAT and ERRMSG
- Allocatable assignment with automatic reallocation works
- MOVE_ALLOC works
- Array sections create correct descriptors
- ALLOCATED intrinsic works
- Large arrays automatically placed on heap (no stack overflow)
- Automatic deallocation at scope exit (no leaks)
- All memory operations tested, including stress tests
- `cargo test` memory management tests pass
