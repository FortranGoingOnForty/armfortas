# Sprint 24: Runtime — Basic I/O

## Prerequisites
Sprint 23 (strings — I/O heavily uses string operations)

## Goals
Implement Fortran's I/O subsystem for the most common patterns: list-directed I/O (`print *`, `read *`), unit management (`open`/`close`), and basic formatted output. Fortran I/O is essentially a small database engine — this sprint handles the common cases; Sprint 25 covers the rest.

## Deliverables

### 1. I/O Unit Table
Fortran I/O uses unit numbers (integers) to refer to files:

```rust
struct IoState {
    units: HashMap<i32, Unit>,
    next_newunit: i32,        // for OPEN(NEWUNIT=u)
}

struct Unit {
    number: i32,
    file: Option<File>,        // None for stdin/stdout/stderr
    filename: String,
    status: UnitStatus,
    access: Access,            // sequential, direct, stream
    form: Form,                // formatted, unformatted
    action: Action,            // read, write, readwrite
    position: Position,        // beginning, end, append
    recl: Option<i64>,         // record length (direct access)
    pad: bool,                 // pad input with blanks?
    delim: Delim,              // none, quote, apostrophe
    decimal: DecimalMode,      // point or comma
    encoding: Encoding,        // default or utf-8
    current_rec: i64,          // current record position
}
```

**Preconnected units:**
- Unit 5 → stdin
- Unit 6 → stdout
- Unit 0 → stderr
- `*` in I/O statements → unit 5 (read) or 6 (write)

### 2. List-Directed Output (PRINT *, WRITE(*,*))
The most common I/O pattern:
```fortran
print *, 'Hello'              ! character
print *, 42                   ! integer
print *, 3.14                 ! real
print *, .true.               ! logical
print *, x, y, z              ! multiple items
print *, 'x =', x, 'y =', y  ! mixed types
print *, array                 ! entire array
```

List-directed output rules:
- Leading space on each record (carriage control character, historically)
- Values separated by spaces (implementation-defined spacing)
- Integers: no leading zeros, right-justified in reasonable field
- Reals: implementation-defined format (we use a sensible default)
- Logicals: `T` or `F`
- Characters: no delimiters in list-directed
- Complex: `(real, imag)`
- Arrays: elements separated by spaces, possibly multiple lines

```rust
#[no_mangle]
pub extern "C" fn __afs_write_star_int(unit: i32, val: i64, kind: i32) { ... }
#[no_mangle]
pub extern "C" fn __afs_write_star_real(unit: i32, val: f64, kind: i32) { ... }
#[no_mangle]
pub extern "C" fn __afs_write_star_char(unit: i32, ptr: *const u8, len: i64) { ... }
#[no_mangle]
pub extern "C" fn __afs_write_star_logical(unit: i32, val: i32) { ... }
#[no_mangle]
pub extern "C" fn __afs_write_star_newline(unit: i32) { ... }  // end of print statement
```

### 3. List-Directed Input (READ *, READ(*,*))
```fortran
read *, x, y, z           ! read three values from stdin
read *, name               ! read a string
read(10, *) values(:)      ! read from file unit 10
```

List-directed input rules:
- Values separated by commas or whitespace
- `/` terminates input (remaining variables unchanged)
- Null value (consecutive commas): variable unchanged
- Repeat count: `3*1.0` means three `1.0` values
- Strings: delimited by quotes, or undelimited (read to separator)
- Logical: `T`, `F`, `.TRUE.`, `.FALSE.`

```rust
#[no_mangle]
pub extern "C" fn __afs_read_star_int(unit: i32, val: *mut i64, kind: i32, iostat: *mut i32) { ... }
#[no_mangle]
pub extern "C" fn __afs_read_star_real(unit: i32, val: *mut f64, kind: i32, iostat: *mut i32) { ... }
#[no_mangle]
pub extern "C" fn __afs_read_star_char(unit: i32, ptr: *mut u8, len: i64, iostat: *mut i32) { ... }
```

### 4. OPEN Statement
```fortran
open(unit=10, file='data.txt', status='old', action='read', iostat=ios)
open(newunit=u, file='output.dat', status='replace', form='unformatted')
```

```rust
#[no_mangle]
pub extern "C" fn __afs_open(
    unit: i32,
    filename: *const u8, filename_len: i64,
    status: *const u8, status_len: i64,        // 'old', 'new', 'replace', 'scratch', 'unknown'
    access: *const u8, access_len: i64,        // 'sequential', 'direct', 'stream'
    form: *const u8, form_len: i64,            // 'formatted', 'unformatted'
    action: *const u8, action_len: i64,        // 'read', 'write', 'readwrite'
    recl: i64,                                  // record length (direct access)
    iostat: *mut i32,
    newunit: *mut i32,                          // output: assigned unit number
) { ... }
```

### 5. CLOSE Statement
```rust
#[no_mangle]
pub extern "C" fn __afs_close(
    unit: i32,
    status: *const u8, status_len: i64,    // 'keep' or 'delete'
    iostat: *mut i32,
) { ... }
```

### 6. Basic Formatted Output
```fortran
write(*, '(A, I4)') 'Count: ', n
write(*, '(3F10.4)') x, y, z
write(*, '(A, ES15.8)') 'Value: ', result
```

Implement the most common format descriptors:
- `I` — integer (with width and optional minimum digits)
- `F` — fixed-point real
- `E` — exponential real
- `ES` — scientific (engineering style, 1.0-9.999 mantissa)
- `A` — character
- `L` — logical
- `X` — space
- `/` — newline
- `'text'` — literal text in format

```rust
struct FormatEngine {
    descriptors: Vec<FormatDescriptor>,
    position: usize,
    repeat_stack: Vec<RepeatState>,
}

impl FormatEngine {
    fn format_value(&mut self, val: &IoValue) -> String { ... }
}
```

### 7. IOSTAT and Error Handling
I/O operations return status through IOSTAT:
- `iostat = 0` → success
- `iostat < 0` → end-of-file or end-of-record
- `iostat > 0` → error

Named constants from `iso_fortran_env`:
- `IOSTAT_END` = -1
- `IOSTAT_EOR` = -2

If IOSTAT is not provided and an error occurs → runtime error (stop program).

If ERR= label is specified → jump to that label on error.

## Testing Strategy

### Print Tests
Compile programs that print various types, capture stdout, compare with expected output:
- Integers of various kinds
- Reals (check reasonable formatting)
- Characters
- Logicals
- Arrays
- Mixed types in one statement

### Read Tests
Pipe input to compiled programs, verify parsed values:
```bash
echo "1 2 3" | ./test_read
echo "1.5, 2.5, 3.5" | ./test_read_real
```

### File I/O Tests
Open, write, close, reopen, read, verify:
```fortran
open(10, file='test.dat')
write(10, *) 42
close(10)
open(10, file='test.dat', status='old')
read(10, *) x
! x must equal 42
```

### Formatted Output Tests
Compare formatted output against expected strings:
```fortran
write(*, '(I5)') 42          ! "   42"
write(*, '(F8.3)') 3.14159   ! "   3.142"
write(*, '(ES12.5)') 1234.5  ! " 1.23450E+03"
```

### IOSTAT Tests
- Read past end of file → iostat < 0
- Open non-existent file with status='old' → iostat > 0
- Verify program doesn't crash when IOSTAT is provided

## Definition of Done
- `print *` works for all intrinsic types
- `read *` works for integer, real, character
- OPEN/CLOSE work with common options
- NEWUNIT works
- Basic formatted output (I, F, E, ES, A, L, X, /) works
- IOSTAT error handling works
- Preconnected units (stdin, stdout, stderr) work
- File I/O round-trip (write then read) works
- `cargo test` I/O tests pass
