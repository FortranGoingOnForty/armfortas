# Sprint 11: Parser — Advanced Features (I/O, Derived Types, Interfaces)

## Prerequisites
Sprint 10 (subprograms & modules)

## Goals
Complete the parser by handling Fortran's remaining complex statement types: I/O statements (which have their own mini-grammar), advanced derived type features, and other statement types not yet covered. After this sprint, the parser handles all of F2018.

## Deliverables

### 1. I/O Statements
Fortran I/O is essentially a domain-specific language embedded in the main language:

```fortran
! PRINT
print *, 'Hello'
print '(A,I4)', 'Count: ', n
print fmt, x, y, z

! WRITE
write(*, *) x, y, z
write(unit=10, fmt='(3F10.4)', iostat=ios, err=100) a, b, c
write(unit=output_unit, nml=my_namelist)
write(char_var, '(I10)') n    ! internal write

! READ
read *, x, y
read(10, '(A)') line
read(unit=input_unit, fmt=*, iostat=ios, end=200) values
read(char_var, *) parsed_value  ! internal read

! OPEN
open(unit=10, file='data.txt', status='old', action='read', iostat=ios)
open(newunit=u, file=filename, form='unformatted', access='stream')

! CLOSE
close(10)
close(unit=u, status='delete', iostat=ios)

! INQUIRE
inquire(file='test.dat', exist=file_exists)
inquire(unit=10, opened=is_open, size=file_size)
inquire(iolength=rec_len) x, y, z

! File positioning
rewind(10)
backspace(unit=u, iostat=ios)
endfile(10)
flush(10)

! WAIT (F2003, async I/O)
wait(unit=10, iostat=ios)
```

```rust
enum IoStmt {
    Print { format: FormatSpec, items: Vec<IoItem> },
    Write { controls: Vec<IoControl>, items: Vec<IoItem> },
    Read { controls: Vec<IoControl>, items: Vec<IoItem> },
    Open { specs: Vec<ConnectSpec> },
    Close { specs: Vec<CloseSpec> },
    Inquire { specs: Vec<InquireSpec>, items: Option<Vec<IoItem>> },
    Rewind { specs: Vec<PositionSpec> },
    Backspace { specs: Vec<PositionSpec> },
    Endfile { specs: Vec<PositionSpec> },
    Flush { specs: Vec<FlushSpec> },
}

enum FormatSpec {
    Star,                    // * (list-directed)
    Label(u64),             // FORMAT statement label
    Expr(Expr),             // character expression
}

enum IoItem {
    Expr(Expr),
    ImpliedDo { items: Vec<IoItem>, var: String, start: Expr, end: Expr, step: Option<Expr> },
}
```

### 2. FORMAT Statement
```fortran
100 format(3I10, 2F12.4, A, /, 'Header:', T20, ES15.8)
```

FORMAT has its own mini-language of edit descriptors:
- Data: `I`, `F`, `E`, `ES`, `EN`, `G`, `D`, `A`, `L`, `B`, `O`, `Z`
- Control: `/`, `:`, `T`, `TL`, `TR`, `X`, `SS`, `SP`, `S`, `BN`, `BZ`
- Character string: `'text'`
- Repeat: `3I10`, `2(I5, F8.2)`
- Unlimited repeat: `*(I5)` (F2008)

The FORMAT parser is essentially a separate sub-parser.

### 3. ALLOCATE / DEALLOCATE
```fortran
allocate(a(n), b(m,k), stat=ios, errmsg=msg)
allocate(character(len=n) :: str)
allocate(base_type :: polymorphic_var)
allocate(x, source=template)
allocate(y, mold=template)
deallocate(a, b, stat=ios, errmsg=msg)
```

### 4. CALL Statement
```fortran
call subroutine_name(arg1, arg2, keyword=value)
call obj%method(args)
call indirect_call(arg)   ! through procedure pointer
```

### 5. NAMELIST
```fortran
namelist /input_data/ x, y, z, name, values
namelist /output_data/ result, error_code
```

### 6. Executable Statements Roundup
```fortran
! NULLIFY
nullify(ptr1, ptr2)

! SYNC (coarray, F2008)
sync all
sync images(partner)
sync memory

! EVENT (F2018)
event post(ev)
event wait(ev)

! LOCK/UNLOCK (F2018)
lock(lock_var)
unlock(lock_var)

! FAIL IMAGE (F2018)
fail image

! CHANGE TEAM (F2018)
change team(team_var)
    ! ...
end team
```

### 7. Procedure Pointers and Procedure Components
```fortran
procedure(interface_name), pointer :: proc_ptr => null()
proc_ptr => actual_procedure
call proc_ptr(args)
```

## Testing Strategy

### I/O Statement Tests
Parse every I/O statement form with all keyword combinations:
- Write/read with and without unit
- All OPEN specifiers
- INQUIRE by file, by unit, by output list
- Internal I/O (read/write to character variable)

### FORMAT Tests
Parse format strings with all edit descriptors:
- Nested repeat groups
- All data descriptors with width/decimal/exponent
- All control descriptors
- Character string edit descriptors

### ALLOCATE Tests
- Simple allocation
- With source/mold
- Typed allocation (polymorphic)
- Character length allocation
- Coarray allocation

### Integration: Parse All of fortsh
At this point the parser should handle everything in fortsh. Parse all 55 files and verify zero parser errors. This is the parser's graduation test.

## Key Technical Notes

### I/O Control List Ambiguity
`read(10, *, iostat=ios) x` — is `10` the unit and `*` the format, or are these positional arguments? Convention: first argument is unit, second is format when positional. Keyword form removes ambiguity.

### WRITE vs Function Call
`write(x, *) y` could look like a function call to the parser. Context resolves it: `write` at statement position is always an I/O statement.

## Definition of Done
- All I/O statements parse with all control specifiers
- FORMAT statements parse with all edit descriptors
- ALLOCATE/DEALLOCATE parse with all options
- CALL statements parse (including method calls)
- NAMELIST parse
- All remaining F2018 statement types parse
- **All 55 fortsh source files parse with zero errors** ← parser graduation
- `cargo test` all parser tests pass
