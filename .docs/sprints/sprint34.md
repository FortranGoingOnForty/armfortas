# Sprint 34: fortsh Compilation — Full Build

## Prerequisites
Sprint 33 (core modules compile)

## Goals
Compile the entire fortsh codebase — all 55 .f90 files and 3 C files — into a working binary that passes fortsh's test suite. This is the definition of done for the entire ARMFORTAS project.

## Deliverables

### 1. Remaining Module Compilation
Compile all remaining fortsh source directories:

**parsing/** (lexer, grammar parser, AST, glob patterns):
- Heavy use of allocatable strings and arrays
- Recursive descent parser (recursive functions)
- Pattern matching (glob patterns with wildcards)

**execution/** (AST executor, builtins, job control, pipelines):
- Process management (fork, exec via iso_c_binding)
- Signal handling
- Pipe and file descriptor management
- ~50 builtin implementations (5000+ lines)

**scripting/** (variables, expansion, substitution, completion):
- Heavy string manipulation
- Parameter expansion (complex string operations)
- Pattern matching

**io/** (readline, syntax highlighting, heredoc, fd redirection):
- The 8800-line readline implementation
- Terminal control (ANSI escape sequences)
- Autosuggestions and tab completion

**fortsh.f90** (main REPL loop, 1335 lines)

### 2. Full Link
```bash
# Compile all Fortran sources
for f in $(find src -name '*.f90'); do
    afs --std=f2018 -O2 -c "$f" -I./build/modules -J./build/modules -o "build/$(basename $f .f90).o"
done

# Compile C sources
for f in src/c_interop/*.c; do
    clang -c "$f" -o "build/$(basename $f .c).o"
done

# Link everything
afs build/*.o -o fortsh -lSystem
```

### 3. Test Suite Execution
Run fortsh's existing test suites against the afs-compiled binary:

- **POSIX compliance**: 3,632+ tests (23 suites)
- **Builtin tests**: 850+
- **Integration tests**: 482
- **Stress tests**: 204
- **Interactive PTY tests**: 180+

Goal: **100% pass rate on all tests**. Any failure is a compiler bug to fix.

### 4. The Zero-Workarounds Test
fortsh currently has platform-specific workarounds for gfortran/flang ARM64 bugs:
- C string library routing
- Safe string assignment wrappers
- C-backed buffer operations
- write_stdout/write_stderr I/O helpers

Compile fortsh with these workarounds **disabled** (using the pure Fortran code paths). If our compiler is correct, the workarounds are unnecessary. This is the ultimate proof.

### 5. Performance Comparison
Benchmark the afs-compiled fortsh against the gfortran/flang-compiled version:
- Shell startup time
- Command execution latency
- Script execution throughput (run a benchmark script)
- Memory usage

We don't need to be faster (though we might be with -O2/O3), but we should not be dramatically slower.

### 6. Bug Fix Sprint
This sprint will inevitably uncover many compiler bugs. The workflow:
1. Attempt compilation
2. Hit bug → minimal reproduction → fix → regression test → retry
3. Repeat until clean compilation
4. Run tests
5. Hit runtime bug → debug → fix → regression test → retry
6. Repeat until all tests pass

Track everything in `.docs/fortsh_campaign/bugs.md` with categories:
- Parser bugs (misparse of valid Fortran)
- Type system bugs (incorrect type inference/checking)
- Codegen bugs (wrong ARM64 instructions)
- Runtime bugs (I/O, string, memory)
- Linker integration bugs

### 7. The Final Binary
```bash
$ ./fortsh        # the afs-compiled fortsh
fortsh $ echo "Hello from ARMFORTAS-compiled fortsh!"
Hello from ARMFORTAS-compiled fortsh!
fortsh $ exit
```

## Testing Strategy

### Progressive Compilation
Don't try to compile everything at once. Work directory by directory:
1. common/ (Sprint 33 — done)
2. system/
3. parsing/
4. scripting/
5. execution/
6. io/
7. fortsh.f90 (main)

At each stage, compile, link what's available, test what's possible.

### Test Suite Execution
Run the full fortsh test suite:
```bash
cd fortsh
FORTSH=./fortsh_afs make test
```

Track pass/fail rates as we fix bugs:
- Day 1: maybe 60% pass
- Day N: 100% pass

### Memory Testing
Run under memory sanitizer (or our own runtime tracking):
- No leaks in long-running shell session
- No buffer overflows
- No use-after-free

### Comparison Testing
Run the same test suite against gfortran-compiled and afs-compiled fortsh. Results must be identical (or our version must be more correct where gfortran has known bugs).

## Definition of Done
- All 55 .f90 files compile with `afs --std=f2018`
- All 3 C files compile with `clang` and link correctly
- fortsh binary runs and presents a shell prompt
- **100% pass rate on POSIX compliance tests** (3,632+ tests)
- **100% pass rate on builtin tests** (850+)
- **100% pass rate on integration tests** (482)
- **100% pass rate on stress tests** (204)
- fortsh runs without the macOS ARM64 workarounds
- Every compiler bug found has a regression test
- Performance within 2x of gfortran-compiled version

**When this sprint is done, ARMFORTAS has achieved its mission.**
