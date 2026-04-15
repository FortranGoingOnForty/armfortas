# Sprint 35: Hardening & Polish

## Prerequisites
Sprint 34 (fortsh compiles and passes tests)

## Goals
The compiler works. Now make it robust, helpful, and pleasant to use. This sprint focuses on error message quality, edge case handling, documentation, and preparing ARMFORTAS for use beyond fortsh.

## Deliverables

### 1. Error Message Quality Audit
Review all error messages in the compiler for:
- **Clarity**: Does a Fortran programmer (not a compiler developer) understand the message?
- **Location accuracy**: Does the caret point to the right token?
- **Suggestions**: Where possible, suggest a fix:
  ```
  error: undeclared variable 'conut' — did you mean 'count'?
  error: missing '::' in declaration with attributes
  error: 'allocate' requires 'allocatable' or 'pointer' attribute
  ```
- **Context**: Show the relevant source lines, with color

### 2. Internal Compiler Error (ICE) Handling
When the compiler itself has a bug:
```
INTERNAL COMPILER ERROR in codegen/arm64.rs:342
  while compiling: src/parsing/grammar_parser.f90:1245
  
  This is a bug in ARMFORTAS. Please report it at:
  https://github.com/FortranGoingOnForty/armfortas/issues
  
  Include this information:
  - ARMFORTAS version: 0.1.0
  - Platform: aarch64-apple-darwin24.5.0
  - Source file: src/parsing/grammar_parser.f90
  - Error: assertion failed: register class mismatch
```

Never show a raw Rust panic to the user. Catch all panics and format them as ICE reports.

### 3. Edge Case Handling
Test and fix handling of:
- Empty source files
- Source files with only comments
- Extremely long lines (> 10,000 characters)
- Deeply nested constructs (100+ levels)
- Extremely large arrays in declarations
- Unicode in comments (valid Fortran)
- BOM (byte order mark) at file start
- Mixed line endings (CRLF, LF, CR)
- Source files without trailing newline
- Extremely long identifiers

### 4. Diagnostic Enhancements
- **Note** diagnostics for additional context:
  ```
  error: type mismatch in subroutine call 'process'
    argument 'x' has type real(4) but expected real(8)
  note: 'process' declared here:
    module.f90:42: subroutine process(x, y)
  ```
- **Warning groups** that can be enabled/disabled individually
- **Color** auto-detection (on when TTY, off when piped)
- **JSON error output** (for IDE integration): `--diagnostics-format=json`

### 5. Debug Information (DWARF)
When `-g` is specified:
- Emit DWARF debug information in the Mach-O object file
- Source file and line number mapping (so `lldb` can show source)
- Variable names and types (so `lldb` can print locals)
- Function names and signatures

This allows debugging afs-compiled binaries with `lldb`:
```bash
afs -g program.f90 -o program
lldb program
(lldb) breakpoint set --file program.f90 --line 42
(lldb) run
(lldb) print x
```

### 6. Compiler Performance
Profile the compiler itself and optimize hot paths:
- Lexer: should handle 100K+ lines/second
- Parser: should parse fortsh (57K lines) in under 5 seconds
- Full compilation of fortsh: under 60 seconds at -O0, under 120 seconds at -O2

### 7. Documentation
Generate documentation for `docs/` (tracked):
- `docs/user-guide.md` — How to install and use ARMFORTAS
- `docs/language-support.md` — Which Fortran features are supported, per standard
- `docs/cli-reference.md` — Complete CLI flag reference
- `docs/internals.md` — Compiler architecture overview (for contributors)
- `docs/porting.md` — How to port a project from gfortran/flang to ARMFORTAS

### 8. Fuzzing Infrastructure
Set up basic fuzzing for the parser:
- Generate random Fortran-like source
- Feed to the compiler
- Verify no crashes (ICEs)
- Any crash → minimal reproduction → bug fix → regression test

### 9. Broader Test Suite
Beyond fortsh, compile other real-world Fortran code:
- Fortran stdlib (fortran-lang/stdlib) — modern Fortran library
- fpm (fortran-lang/fpm) — Fortran package manager
- Selected BLAS/LAPACK routines — classic numerical Fortran
- Small scientific programs from .refs/

Each successful compilation broadens our confidence.

### 10. Install Target
```bash
make install    # or cargo install
```

Installs:
- `armfortas` binary → `/usr/local/bin/armfortas`
- `afs` symlink → `/usr/local/bin/afs`
- `afs-as` binary → `/usr/local/bin/afs-as`
- `libarmfortas_rt.a` → `/usr/local/lib/libarmfortas_rt.a`
- Built-in `.amod` files → `/usr/local/lib/armfortas/modules/`

## Testing Strategy

### Error Message Snapshot Tests
For a curated set of error programs, capture stderr and compare against golden files. Any change to error messages must be intentional.

### Edge Case Tests
Compile each edge case listed above, verify no crashes.

### Performance Benchmarks
Time the compilation of fortsh at -O0 and -O2. Track these numbers to prevent regressions.

### Fuzzing
Run parser fuzzer for at least 1 hour with no crashes.

### External Code Compilation
Compile at least 3 external Fortran projects beyond fortsh.

## Definition of Done
- Error messages are clear, accurate, and helpful
- ICEs produce formatted bug reports (no raw panics)
- All edge cases handled without crashes
- `-g` produces DWARF debug info usable with lldb
- Compiler performance meets targets
- User-facing documentation written in docs/
- At least 3 external Fortran codebases compile successfully
- Install target works
- Parser fuzzer runs 1 hour with no crashes
- **ARMFORTAS is ready for real-world use**
