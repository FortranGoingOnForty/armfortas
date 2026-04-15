# Sprint 33: fortsh Compilation — Core Modules

## Prerequisites
All prior sprints (the compiler is feature-complete)

## Goals
Begin compiling the actual fortsh codebase with ARMFORTAS. Start with the core modules (common/, system/) that have the fewest dependencies and the most straightforward Fortran. This sprint is where theory meets reality — every compiler bug we have will surface here.

## Deliverables

### 1. Compilation Target: fortsh/src/common/
The `common/` directory contains foundational modules:
- `types.f90` — Core type definitions (shell_state_t, command_t, etc.)
- `error_handling.f90` — Error reporting infrastructure
- `string_pool.f90` — Memory pooling for strings
- `buffer_ops.f90` — Buffer operations (routes through C on macOS ARM64 in gfortran build)
- `performance.f90` — Performance monitoring
- `memory_profiler.f90` ��� Memory tracking
- `memory_dashboard.f90` — Memory dashboard display

Strategy:
1. Attempt to compile each file
2. Record every failure (parser error, type error, codegen error, runtime crash)
3. Fix each bug in the compiler
4. Re-attempt compilation
5. Repeat until all files compile

### 2. Compilation Target: fortsh/src/system/
System-level modules:
- Signal handling (uses iso_c_binding)
- POSIX syscall wrappers (heavy iso_c_binding)
- Environment variable access

These exercise iso_c_binding heavily — Sprint 27's work gets a real workout.

### 3. Bug Triage Process
For each compilation failure:
```
File: types.f90
Line: 45
Phase: parser / sema / codegen / runtime
Error: <exact error message>
Root cause: <analysis>
Fix: <what to change in the compiler>
Regression test: <test case extracted from the failure>
```

Track all bugs in `.docs/fortsh_campaign/bugs.md`.

### 4. fortsh Build System Integration
Create a build script that compiles fortsh with `afs`:
```bash
#!/bin/bash
AFS=/path/to/afs
FFLAGS="--std=f2018 -O1"

# Compile modules in dependency order
$AFS $FFLAGS -c src/common/types.f90
$AFS $FFLAGS -c src/common/error_handling.f90
# ... etc
```

Eventually this becomes a Makefile that mirrors fortsh's existing build system but uses `afs` instead of `gfortran`/`flang-new`.

### 5. C Interop Compilation
fortsh has 3 C files:
- `src/c_interop/fortsh_strings.c`
- `src/c_interop/fd_wrapper.c`
- `src/c_interop/terminal_size.c`

These are compiled with `clang` (not our compiler — we compile Fortran, not C). But the Fortran interface modules that declare the BIND(C) interfaces must compile with `afs` and produce ABI-compatible calls.

Test: compile the Fortran interface module, compile the C file with clang, link together, call a C function from Fortran, verify it works.

### 6. Regression Test Extraction
Every bug found during this sprint becomes a minimal test case added to `tests/regression/`:
```fortran
! tests/regression/issue_042_deferred_char_in_type.f90
! Bug: compiler ICE when derived type has deferred-length character component
program test
    type :: container
        character(:), allocatable :: name
    end type
    type(container) :: c
    c%name = 'hello'
    if (c%name /= 'hello') stop 1
end program
```

## Testing Strategy

### Per-File Compilation Test
For each fortsh source file in common/ and system/:
1. `afs -c file.f90` succeeds (no compiler errors/crashes)
2. Object file is valid (verifiable with `nm` and `otool`)

### Linking Test
Link all compiled common/ and system/ object files together with the C files:
```bash
afs -c src/common/*.f90
clang -c src/c_interop/*.c
ar rcs libfortsh_common.a common/*.o c_interop/*.o
```

### Functional Test
Write a small test program that:
1. USEs the common modules
2. Creates derived types
3. Calls string pool functions
4. Calls system functions
5. Runs and produces correct output

## Key Technical Notes

### Expected Bug Categories
Based on fortsh's heavy use of:
- **Allocatable strings**: most likely source of bugs (our Sprint 23 work will be tested hard)
- **Derived types with allocatable components**: descriptor management
- **iso_c_binding**: ABI compatibility with clang
- **Module dependencies**: circular-ish dependency chains
- **Large modules**: types.f90 with many type definitions

### The Buffer Ops Problem
fortsh's `buffer_ops.f90` routes string operations through C on macOS ARM64 (working around gfortran bugs). With ARMFORTAS, these workarounds should be unnecessary — we can compile the "pure Fortran" versions of these operations. This is a key validation: if our compiler handles strings correctly, the C workarounds become dead code.

## Definition of Done
- All files in fortsh/src/common/ compile with `afs -c`
- All files in fortsh/src/system/ compile with `afs -c`
- All object files link together without errors
- C interop (BIND(C) functions) works across afs↔clang boundary
- Every compiler bug found is fixed and has a regression test
- Bug tracker `.docs/fortsh_campaign/bugs.md` maintained
- fortsh common/ functional test passes
