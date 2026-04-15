# Sprint 23: Runtime — Strings (The Big One)

## Prerequisites
Sprint 22 (memory management — strings are allocatable under the hood)

## Goals
Implement correct, bulletproof character string handling. This sprint addresses the #1 source of gfortran ARM64 bugs. Every string operation — assignment, concatenation, comparison, substring, trim — must be rock-solid. This is where we prove ARMFORTAS was worth building.

## Deliverables

### 1. String Descriptor
```rust
#[repr(C)]
pub struct StringDescriptor {
    pub data: *mut u8,
    pub len: i64,           // current length in bytes
    pub capacity: i64,      // allocated capacity (for deferred-length)
    pub flags: u32,         // allocated, deferred-length, etc.
}

const STR_ALLOCATED: u32 = 1;
const STR_DEFERRED: u32 = 2;    // deferred-length (character(len=:))
```

### 2. Fixed-Length Character Operations
```fortran
character(20) :: name
name = 'Alice'       ! right-padded with spaces: "Alice               "
```

Fixed-length strings are simple: stack-allocated buffer, assignment pads with spaces or truncates.

```rust
#[no_mangle]
pub extern "C" fn __afs_assign_char_fixed(
    dest: *mut u8,
    dest_len: i64,
    src: *const u8,
    src_len: i64,
) {
    let copy_len = src_len.min(dest_len);
    unsafe {
        ptr::copy_nonoverlapping(src, dest, copy_len as usize);
        // Pad remainder with spaces
        ptr::write_bytes(dest.add(copy_len as usize), b' ', (dest_len - copy_len) as usize);
    }
}
```

### 3. Deferred-Length Character (The Critical Path)
```fortran
character(:), allocatable :: s
s = 'hello'              ! allocate 5 bytes, copy
s = s // ' world'        ! allocate 11 bytes, concat, free old
s = trim(s) // '!'       ! compute new value, reallocate, assign
```

**This is where gfortran corrupts memory on ARM64.** Our implementation:

```rust
#[no_mangle]
pub extern "C" fn __afs_assign_char_deferred(
    desc: *mut StringDescriptor,
    src: *const u8,
    src_len: i64,
) {
    let desc = unsafe { &mut *desc };
    
    // If source might overlap with dest (e.g., s = s(2:5))
    // we must handle this carefully: allocate new, copy, free old
    
    if src_len > desc.capacity || desc.data.is_null() {
        // Need (re)allocation
        let new_data = alloc(src_len);
        unsafe {
            ptr::copy_nonoverlapping(src, new_data, src_len as usize);
        }
        if !desc.data.is_null() && (desc.flags & STR_ALLOCATED != 0) {
            dealloc(desc.data, desc.capacity);
        }
        desc.data = new_data;
        desc.capacity = src_len;
    } else {
        // Fits in existing buffer
        unsafe {
            ptr::copy_nonoverlapping(src, desc.data, src_len as usize);
        }
    }
    desc.len = src_len;
    desc.flags |= STR_ALLOCATED;
}
```

**Key safety property**: We always allocate new memory before freeing old memory. This prevents use-after-free when the source is a substring of the destination.

### 4. String Concatenation
```fortran
c = a // b
```

```rust
#[no_mangle]
pub extern "C" fn __afs_concat(
    result: *mut u8,      // pre-allocated by caller
    a: *const u8, a_len: i64,
    b: *const u8, b_len: i64,
) {
    unsafe {
        ptr::copy_nonoverlapping(a, result, a_len as usize);
        ptr::copy_nonoverlapping(b, result.add(a_len as usize), b_len as usize);
    }
}
```

For deferred-length results, codegen allocates a temporary of length `a_len + b_len`, calls concat, then assigns to the deferred-length target.

### 5. String Comparison
```fortran
if (a == b) then        ! character comparison
if (a < b) then         ! lexicographic comparison
```

```rust
#[no_mangle]
pub extern "C" fn __afs_compare_char(
    a: *const u8, a_len: i64,
    b: *const u8, b_len: i64,
) -> i32 {
    // Fortran comparison: shorter string is padded with spaces
    let max_len = a_len.max(b_len) as usize;
    for i in 0..max_len {
        let ac = if i < a_len as usize { unsafe { *a.add(i) } } else { b' ' };
        let bc = if i < b_len as usize { unsafe { *b.add(i) } } else { b' ' };
        if ac < bc { return -1; }
        if ac > bc { return 1; }
    }
    0
}
```

### 6. String Intrinsics
Implement in the runtime:

```rust
// TRIM: return string with trailing spaces removed
pub extern "C" fn __afs_trim(src: *const u8, src_len: i64, result_len: *mut i64) -> *const u8

// LEN_TRIM: length without trailing spaces
pub extern "C" fn __afs_len_trim(src: *const u8, src_len: i64) -> i64

// ADJUSTL: left-justify (remove leading spaces, pad trailing)
pub extern "C" fn __afs_adjustl(dest: *mut u8, src: *const u8, len: i64)

// ADJUSTR: right-justify
pub extern "C" fn __afs_adjustr(dest: *mut u8, src: *const u8, len: i64)

// INDEX: find substring
pub extern "C" fn __afs_index(str: *const u8, str_len: i64, sub: *const u8, sub_len: i64, back: i32) -> i64

// SCAN: find any character from set
pub extern "C" fn __afs_scan(str: *const u8, str_len: i64, set: *const u8, set_len: i64, back: i32) -> i64

// VERIFY: find character not in set
pub extern "C" fn __afs_verify(str: *const u8, str_len: i64, set: *const u8, set_len: i64, back: i32) -> i64

// REPEAT: repeat string n times
pub extern "C" fn __afs_repeat(src: *const u8, src_len: i64, ncopies: i64, dest: *mut u8)

// CHAR: integer to character
pub extern "C" fn __afs_char(i: i32) -> u8

// ICHAR: character to integer
pub extern "C" fn __afs_ichar(c: u8) -> i32

// LGE, LGT, LLE, LLT: lexicographic comparison per ASCII
pub extern "C" fn __afs_lge(a: *const u8, a_len: i64, b: *const u8, b_len: i64) -> i32
```

### 7. Substring Operations
```fortran
s(3:7)              ! substring reference — no copy, just adjust pointer and length
s(3:7) = 'hello'    ! substring assignment — write into existing buffer
```

Substring reference is lowered to `{data + (start-1), end - start + 1}` — a new descriptor pointing into the original buffer. No allocation needed.

Substring assignment is a bounded memcpy into the existing buffer.

## Testing Strategy

### The gfortran Bug Reproduction Suite
Write test cases that specifically trigger every known gfortran ARM64 string bug:

1. **Deferred-length allocatable loss** — assign to `character(:), allocatable`, verify value persists
2. **Allocatable string > 16 bytes** — assign strings of various lengths (16, 17, 32, 64, 128, 256, 1024)
3. **String assignment corruption** — assign, modify, re-assign, verify no corruption
4. **Empty string assignment** — `s = ''`, verify len=0, no crash
5. **Substring slice** — `s(3:7)`, verify correct characters
6. **Flush in loop with strings** — allocatable string inside a loop with flush, verify no heap corruption

Each test: compile with `afs`, run, verify output. Then compile same test with gfortran on ARM64 — if it crashes there and works with us, we've proven our point.

### Stress Tests
- Concatenate strings in a loop 10,000 times (growing string)
- Assign random-length strings repeatedly (exercises reallocation)
- Substring operations on large strings (1MB)
- Mixed fixed-length and deferred-length operations

### Memory Safety Tests
- No buffer overflows (write beyond allocated length)
- No use-after-free (especially in `s = s // 'more'` patterns)
- No memory leaks (every allocation eventually freed)
- Self-assignment: `s = s` must not corrupt

### Encoding Tests
Fortran strings are byte sequences. Verify correct handling of:
- ASCII text
- Null bytes embedded in strings
- High bytes (> 127)

## Key Technical Notes

### Why gfortran Breaks
gfortran's ARM64 string bugs are in how it manages allocatable character descriptors. The descriptor lives in a register, but the pointed-to data is on the heap. After certain operations (especially in loops with I/O), the descriptor's length field gets corrupted because the register was clobbered by a callee-saved register that wasn't properly saved.

We avoid this entirely: our descriptors are always in memory (stack slots), not registers. The register allocator only puts the descriptor *pointer* in a register, never the descriptor itself. This is slightly less optimal but eliminates the class of bug entirely.

### Self-Referential Assignment
```fortran
s = s(2:) // s(1:1)    ! rotate first character to end
```
The source reads from `s` while the destination writes to `s`. We must compute the full RHS value into a temporary before assigning to `s`. The codegen handles this by materializing all string expressions into temporaries.

## Definition of Done
- Fixed-length character assignment works (with padding and truncation)
- Deferred-length character assignment works (with reallocation)
- Concatenation works
- All comparison operators work
- All string intrinsics (trim, adjustl, adjustr, index, scan, verify, repeat) work
- Substring reference and assignment work
- Self-referential assignment safe (s = s // 'x')
- Every known gfortran ARM64 string bug is a non-issue
- Memory leak-free under stress testing
- `cargo test` string tests pass (target: 100+ string-specific tests)
