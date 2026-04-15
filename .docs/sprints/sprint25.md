# Sprint 25: Runtime — Advanced I/O

## Prerequisites
Sprint 24 (basic I/O)

## Goals
Complete the I/O subsystem with everything not covered in Sprint 24: the full format descriptor set, direct and stream access, unformatted I/O, internal I/O (read/write to strings), INQUIRE, NAMELIST I/O, and file positioning operations.

## Deliverables

### 1. Complete Format Descriptor Set

**Data descriptors (remaining):**
- `G` — generalized (uses F or E depending on value magnitude)
- `D` — double precision exponential (legacy, like E but with D exponent letter)
- `EN` — engineering notation (exponent is multiple of 3)
- `B` — binary integer
- `O` — octal integer
- `Z` — hexadecimal integer
- `DT` — derived type I/O (F2003)

**Control descriptors (remaining):**
- `T`, `TL`, `TR` — tab to position, tab left, tab right
- `:` — format termination (stop if no more items)
- `S`, `SP`, `SS` — sign control (suppress, show, default)
- `BN`, `BZ` — blank interpretation (null or zero)
- `RU`, `RD`, `RZ`, `RN`, `RC`, `RP` — rounding modes (F2003)
- `DC`, `DP` — decimal comma/point mode (F2003)

**Repeat groups:**
```fortran
100 format(3(I5, ', '), I5)           ! repeat group
200 format(*(I5, :, ', '))            ! unlimited repeat (F2008)
```

### 2. Direct Access I/O
```fortran
open(10, file='records.dat', access='direct', recl=100, form='formatted')
write(10, rec=5, fmt='(A)') 'Record number 5'
read(10, rec=5, fmt='(A)') buffer
```

Records are fixed-length, randomly accessible by record number. The runtime calculates byte offset: `offset = (rec - 1) * recl`.

### 3. Stream Access I/O (F2003)
```fortran
open(10, file='data.bin', access='stream', form='unformatted')
write(10) x, y, z           ! write at current position
inquire(10, pos=current_pos) ! get current byte position
read(10, pos=100) value      ! read from specific byte position
```

Stream access treats the file as a flat byte sequence. No record structure.

### 4. Unformatted I/O
```fortran
open(10, file='data.bin', form='unformatted')
write(10) array              ! write binary representation
read(10) array               ! read it back
```

Unformatted sequential records: each record is preceded and followed by a 4-byte record length marker (same convention as gfortran for compatibility with existing binary data files).

### 5. Internal I/O
Read/write to character variables instead of files:
```fortran
character(20) :: buf
integer :: n

! Write to string
write(buf, '(I10)') 42       ! buf = "        42          "

! Read from string
read(buf, *) n                ! n = 42
```

Internal I/O uses the same format engine but operates on a character buffer instead of a file.

### 6. INQUIRE Statement
```fortran
! Inquire by file
inquire(file='test.dat', exist=file_exists, size=file_size)

! Inquire by unit
inquire(unit=10, opened=is_open, name=filename, &
        access=access_type, form=form_type, &
        recl=record_length, position=pos, &
        read=can_read, write=can_write, &
        sequential=seq_ok, direct=dir_ok)

! Inquire by output list (get record length needed)
inquire(iolength=rec_len) x, y, z
```

### 7. File Positioning
```fortran
rewind(10)                    ! go to beginning
backspace(10)                 ! go back one record
endfile(10)                   ! write EOF marker
flush(10)                     ! flush buffers (the one that crashes gfortran!)
```

FLUSH must be rock-solid. gfortran's ARM64 heap corruption occurs during flush in loops. Our implementation:
- Flush calls the OS `fsync` / `fflush` directly
- No interaction with allocatable descriptors
- No hidden allocations during flush

### 8. NAMELIST I/O
```fortran
integer :: nx, ny
real :: dt
character(20) :: method
namelist /config/ nx, ny, dt, method

! Read namelist from file:
open(10, file='config.nml')
read(10, nml=config)

! Write namelist:
write(*, nml=config)
! Output: &CONFIG NX=100, NY=200, DT=0.01, METHOD='rk4' /
```

Namelist format: `&groupname var=value, var=value /`

### 9. Implied DO in I/O
```fortran
write(*, '(10I5)') (i, i=1,100)          ! 100 values, 10 per line
write(*, *) ((a(i,j), j=1,n), i=1,m)     ! nested implied do
read(10, *) (data(i), i=1,n)
```

The I/O runtime must handle implied-do loops, which the codegen lowers to loops that call the I/O routines repeatedly within a single I/O statement context.

## Testing Strategy

### Format Roundtrip Tests
For each format descriptor, write a value, read it back (via internal I/O), verify equality:
```fortran
write(buf, '(ES20.12)') 3.14159265358979d0
read(buf, '(ES20.12)') result
! result must equal 3.14159265358979d0 within precision
```

### Direct Access Tests
Write records out of order, read back in different order, verify correct data.

### Stream Access Tests
Write binary data, seek to specific positions, read back, verify.

### Unformatted Tests
Write arrays of various types, read back, verify bitwise equality.

### Internal I/O Tests
Write to character variable, verify string content. Read from character variable, verify parsed values.

### INQUIRE Tests
Open files with various attributes, INQUIRE and verify each attribute returns correctly.

### FLUSH Stress Test
```fortran
do i = 1, 10000
    write(10, *) i
    flush(10)
end do
```
Must not crash, must not leak, must not corrupt heap. This is the test that kills gfortran.

### NAMELIST Tests
Write namelist, read back, verify all values match.

## Definition of Done
- All format descriptors implemented and tested
- Direct access I/O works
- Stream access I/O works
- Unformatted I/O works (with record markers)
- Internal I/O works
- INQUIRE returns correct values for all specifiers
- REWIND, BACKSPACE, ENDFILE, FLUSH all work
- FLUSH in tight loops does not corrupt anything
- NAMELIST I/O works
- Implied-do in I/O works
- `cargo test` advanced I/O tests pass
