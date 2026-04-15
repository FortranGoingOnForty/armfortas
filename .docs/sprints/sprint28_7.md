# Sprint 28.7: Array Expressions, Sections, WHERE, FORALL

## Prerequisites
Sprint 22 (array descriptors), Sprint 28 (derived types)

## Goals
Implement Fortran's array-level operations: array sections (a(1:10:2)), whole-array assignment, WHERE/FORALL constructs, and elemental operations. These are fundamental to numerical Fortran and required for scientific computing codebases.

## Deliverables

### 1. Array Sections
```fortran
a(1:10)      ! contiguous section
a(1:10:2)    ! strided section
a(:, 2)      ! column slice
a(2:4, 3:5)  ! sub-matrix
```
Lower to runtime calls that create section descriptors from the source array.

### 2. Whole-Array Assignment
```fortran
a = b          ! element-wise copy (same shape)
a = 0          ! broadcast scalar to all elements
a = b + c      ! element-wise addition
```

### 3. WHERE Construct
```fortran
where (a > 0)
    b = sqrt(a)
elsewhere
    b = 0.0
end where
```
Lower to masked element-wise loops.

### 4. FORALL Construct
```fortran
forall (i=1:n, j=1:m, a(i,j) > 0)
    b(i,j) = a(i,j) * 2
end forall
```
Lower to nested loops with mask condition.

### 5. Array Intrinsics (from Sprint 26 deferred)
SUM, PRODUCT, MAXVAL, MINVAL, COUNT, ANY, ALL, MAXLOC, MINLOC, DOT_PRODUCT, MATMUL, RESHAPE, TRANSPOSE, PACK, UNPACK, SPREAD, CSHIFT, EOSHIFT, MERGE, SIZE, SHAPE, LBOUND, UBOUND.

## Definition of Done
- Array sections create correct descriptors
- Whole-array assignment works for same-shape arrays
- WHERE/FORALL lower to correct masked loops
- Core array intrinsics (SUM, SIZE, SHAPE, MATMUL) work end-to-end
