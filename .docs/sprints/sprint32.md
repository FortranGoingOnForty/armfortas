# Sprint 32: CLI Driver & Build Integration

## Prerequisites
Sprint 30 (multi-file compilation), Sprint 31 (multi-standard support)

## Goals
Complete the command-line interface with all standard compiler flags, proper error reporting, and the `afs`/`armfortas` binary names. After this sprint, ARMFORTAS feels like a real compiler from the user's perspective — it handles the same flags and workflows as gfortran or flang.

## Deliverables

### 1. Complete CLI Flag Set
```
USAGE: afs [OPTIONS] <files...>

COMPILATION:
  -c                    Compile to object file only (no linking)
  -S                    Emit assembly text
  -E                    Preprocess only
  -o <file>             Output file name

LANGUAGE:
  --std=<standard>      Fortran standard (f77, f90, f95, f2003, f2008, f2018, f2023)
  -ffree-form           Force free-form source
  -ffixed-form          Force fixed-form source
  -fdefault-integer-8   Make default integer kind 8 bytes
  -fdefault-real-8      Make default real kind 8 bytes
  -fimplicit-none       Force implicit none in all scopes
  -frecursive           Make all procedures recursive by default
  -fbackslash           Interpret backslash in strings as escape
  -fmax-stack-var-size=<n>  Stack variable size threshold (bytes)

OPTIMIZATION:
  -O0                   No optimization (default)
  -O1                   Basic optimization
  -O2                   Standard optimization
  -O3                   Aggressive optimization
  -Os                   Optimize for size

WARNINGS:
  -Wall                 All standard warnings
  -Wextra               Extra warnings
  -Wpedantic            Pedantic standard conformance warnings
  -Wdeprecated          Deprecated feature warnings
  -Werror               Treat warnings as errors
  -Wno-<name>           Disable specific warning

DEBUGGING:
  -g                    Generate debug information (DWARF)
  --emit-ir             Dump IR to stdout
  --emit-ast            Dump AST to stdout
  --emit-tokens         Dump token stream to stdout
  -v                    Verbose output (show compilation phases)
  --time-report         Show time spent in each compilation phase
  -fcheck=bounds        Enable runtime array bounds checking
  -fcheck=all           Enable all runtime checks

DIRECTORIES:
  -I <dir>              Module/include search path
  -J <dir>              Module output directory
  -L <dir>              Library search path
  -l <lib>              Link library

LINKING:
  -shared               Produce shared library
  -static               Static linking
  -rpath <path>         Runtime library path

INFORMATION:
  --version             Print version
  --help                Print help
  -dumpversion          Print version number only
```

### 2. Binary Names
```bash
# Primary name
armfortas hello.f90 -o hello

# Short alias (symlink)
afs hello.f90 -o hello

# Assembler (from submodule)
afs-as input.s -o output.o
```

### 3. Verbose Mode (-v)
```bash
$ afs -v hello.f90 -o hello
armfortas version 0.1.0 (aarch64-apple-darwin)
 preprocessing: hello.f90
 lexing: hello.f90 (free-form)
 parsing: hello.f90 (328 tokens → 45 AST nodes)
 semantic analysis: hello.f90 (0 errors, 0 warnings)
 IR generation: 12 functions, 89 instructions
 optimization: -O0 (no passes)
 codegen: 156 ARM64 instructions
 assembling: hello.o (2048 bytes)
 linking: ld hello.o -larmfortas_rt -lSystem -o hello
```

### 4. Time Report (--time-report)
```bash
$ afs --time-report hello.f90 -o hello
Phase            Time (ms)    %
─────────────────────────────────
Preprocessing        2.1    1%
Lexing               5.3    3%
Parsing             12.7    7%
Semantic Analysis   18.4   10%
IR Generation       15.2    8%
Optimization         0.0    0%
Code Generation     45.3   25%
Assembly            28.1   15%
Linking             54.8   30%
─────────────────────────────────
Total              181.9  100%
```

### 5. Error Output Format
Errors follow a standardized format that editors/IDEs can parse:
```
hello.f90:12:5: error: undefined variable 'xyz'
   12 |     print *, xyz
      |              ^^^
hello.f90:15:3: warning: unused variable 'temp' [-Wunused-variable]
   15 |   real :: temp
      |          ~~~~
```

With color when stdout is a TTY:
- `error:` in red
- `warning:` in yellow  
- `note:` in blue
- Source line and caret in white

### 6. Exit Codes
- 0: Success
- 1: Compilation error (syntax, type, semantic)
- 2: Linker error
- 3: I/O error (can't read input, can't write output)
- 4: Internal compiler error (ICE) — with message asking user to report a bug

### 7. Response Files
For large projects with many files/flags:
```bash
afs @compile_flags.txt
```

Where `compile_flags.txt` contains flags, one per line.

### 8. Make/Build System Integration
The compiler should work seamlessly with Make:
```makefile
FC = afs
FFLAGS = --std=f2018 -O2 -Wall
LDFLAGS = -lSystem

%.o: %.f90
	$(FC) $(FFLAGS) -c $< -o $@

program: main.o module_a.o module_b.o
	$(FC) $(LDFLAGS) $^ -o $@
```

And with CMake (via the standard `CMAKE_Fortran_COMPILER` variable).

### 9. Runtime Library Location
The driver must find `libarmfortas_rt.a` at link time. Search order:
1. `$AFS_RUNTIME_PATH` environment variable
2. Adjacent to the compiler binary: `$(dirname afs)/../lib/libarmfortas_rt.a`
3. Standard install location: `/usr/local/lib/libarmfortas_rt.a`

## Testing Strategy

### Flag Tests
For each flag, verify it's accepted and has the expected effect:
- `-c` → produces .o, no binary
- `-S` → produces .s text
- `-E` → produces preprocessed text
- `-O2` → optimizations applied (compare IR/asm with -O0)
- `--std=f77` → rejects free-form code

### Error Format Tests
Compile programs with errors, capture stderr, verify format matches the spec.

### Exit Code Tests
- Valid program → exit 0
- Syntax error → exit 1
- Missing library → exit 2
- Non-existent input → exit 3

### Integration with Make
Write a Makefile for a multi-file project, run `make`, verify it builds correctly.

### Response File Tests
Create a response file, compile with `@file`, verify same result as passing flags directly.

## Definition of Done
- All listed CLI flags are accepted and functional
- `afs` and `armfortas` both work
- Verbose mode shows compilation progress
- Time report shows phase timings
- Error output is standardized and colorized
- Exit codes are consistent
- Response files work
- Works with Make and standard build systems
- Runtime library found automatically
- `--version` and `--help` produce correct output
- `cargo test` CLI tests pass
