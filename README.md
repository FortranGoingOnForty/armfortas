# armfortas

A Fortran compiler for ARM64 and x86_64, written from scratch in Rust. It does not use LLVM, GCC, or any existing Fortran frontend.

## Why

[fortsh](https://github.com/FortranGoingOnForty/fortsh) is a Fortran 2018 shell, about 64,000 lines long. Compiling it on Apple Silicon turned up problems in both available compilers:

- gfortran has at least seven confirmed critical bugs on macOS ARM64. Most involve allocatable strings, which fortsh uses heavily. The bugs are in GCC's backend.
- flang-new (LLVM) avoids the gfortran bugs but has its own, mostly in C interop and derived type layout. It also needs a separate Homebrew install.
- Both are very large codebases maintained by other people. When a bug shows up in an unusual corner of the AArch64 calling convention, fixing it ourselves isn't practical.

So we wrote a compiler we can fix when it breaks.

## Status

Under active development. The full pipeline, from preprocessor to object file, works on four targets:

- `arm64-macos` (Mach-O, Apple AAPCS64), the original platform
- `x86_64-linux-gnu`, `x86_64-linux-musl`, `x86_64-freebsd` (ELF, SysV AMD64)

```
Pipeline: Source → Preprocessor → Lexer → Parser → AST →
          Sema → SSA IR → Optimizations → ARM64 / x86_64 Codegen →
          afs-as → .o (Mach-O / ELF) → ld → Binary
```

`afs-as` assembles both architectures and writes both object formats in-process, so the default build never calls the system `as`. Linking still goes through the system `ld` by default. Our own linker, `afs-ld`, can be used instead by setting `AFS_LD=1` (or `AFS_LD_PATH`), and it links both ELF and Mach-O.

armfortas compiles and runs real programs. The largest test so far is fpm: armfortas builds fpm, and that fpm rebuilds itself to a byte-identical result.

## Install

armfortas 0.1.x releases are previews. They are meant for testing on real projects and for collecting bug reports.

Apple Silicon macOS:

```bash
brew install FortranGoingOnForty/tap/armfortas
```

Arch Linux x86_64:

```bash
git clone https://aur.archlinux.org/armfortas.git
cd armfortas
makepkg -si
```

The Homebrew package is ARM64 only and the AUR package is x86_64 only. For other targets, build from source. If you download a GitHub release, use the attached `armfortas-VERSION.tar.gz`. The "Source code" archives that GitHub generates automatically leave out submodule contents, so they won't build.

## Build

```bash
git clone --recurse-submodules https://github.com/FortranGoingOnForty/armfortas.git
cd armfortas
cargo build --workspace          # compiler + assembler + runtime
cargo test --workspace           # full test suite
cargo clippy --workspace         # lint
```

Once built:

```bash
target/debug/armfortas hello.f90 -o hello    # compile and link
target/debug/armfortas -c module.f90         # compile to object
target/debug/armfortas -S hello.f90          # emit assembly
target/debug/armfortas --emit-ir hello.f90   # emit IR
target/debug/armfortas --target x86_64-linux-gnu -c hello.f90   # cross-compile to object
```

On Linux, the driver looks for the crt objects it needs (`crtbeginS.o` and so on) in the usual Debian, Red Hat, and Arch GCC locations. On systems with a different layout, such as NixOS, set `AFS_CRT_DIR` to the directory holding those objects and add the matching runtime library directory to `LIBRARY_PATH`.

To install both command names (`armfortas` and `afs`) from a release archive:

```bash
cargo install --path . --locked
armfortas --version
afs --version
```

## Preview limitations

- OpenMP host execution is experimental and limited to the OpenMP 5.2 language baseline. `parallel` regions and the data forms listed in [OPENMP.md](OPENMP.md) run in parallel. Constructs, clauses, and data forms outside that list are rejected at compile time instead of being run serially.
- `afs-as` is the default assembler, but the system linker is still the default linker. `AFS_LD=1` switches to `afs-ld` while we finish bringing it to parity.
- Linux AArch64 is not supported. The only ARM64 target is Apple Silicon macOS, and the ELF targets (Linux and FreeBSD) are x86_64 only.
- `.amod` module files are specific to armfortas. They can't be read by gfortran, flang, or other compilers, and armfortas can't read theirs.
- Language and optimizer coverage is broad but not complete. A 0.1.x release does not claim full Fortran conformance.

To report wrong code, crashes, valid programs that are rejected, or invalid programs that are accepted, use the compiler-bug issue form. Include the output of `armfortas --version`, the full command line, your platform, and the smallest source file that reproduces the problem.

## What works

### Language features (F77 through F2018)

- Free-form and fixed-form source
- Intrinsic types: `integer`, `real`, `double precision`, `complex`, `logical`, `character`
- Derived types with component access, type extension (`EXTENDS`), and type-bound procedures with `PASS`/`NOPASS`
- `FINAL` procedures
- `SELECT TYPE` with `TYPE IS` and `CLASS IS` guards
- Polymorphic dispatch through `CLASS` variables (see [Polymorphic dispatch](#polymorphic-dispatch) below)
- `ALLOCATABLE` scalars and arrays, including allocatable character strings
- `POINTER` and `TARGET` attributes
- `OPTIONAL` arguments and `PRESENT()`
- Array sections and whole-array expressions
- `WHERE` and `FORALL`
- `DO`, `DO WHILE`, and `DO CONCURRENT` with locality specs
- `SELECT CASE` on integer, character, and logical values
- `ASSOCIATE` and `BLOCK`
- `GOTO` and labeled statements
- `EQUIVALENCE` and `COMMON` blocks
- `NAMELIST` I/O
- `SAVE` with static storage
- `VALUE` arguments for `BIND(C)` procedures
- `RECURSIVE` functions and subroutines
- Generic procedures and interfaces
- Operator overloading
- Statement functions
- Arithmetic IF
- `STOP` and `ERROR STOP` with stop codes

### C interoperability (`iso_c_binding`)

The `iso_c_binding` module is built in. It provides the kind parameters (`C_INT`, `C_DOUBLE`, `C_CHAR`, etc.), `C_PTR`, `C_NULL_PTR`, `C_LOC`, and `C_FUNPTR`. `BIND(C)` procedures follow the platform C ABI, including `VALUE` arguments.

### IEEE arithmetic (`ieee_arithmetic`, `ieee_exceptions`)

The value-class functions (`ieee_is_nan`, `ieee_is_finite`, `ieee_is_normal`, `ieee_class`, `ieee_value`, `ieee_unordered`, `ieee_copy_sign`, `ieee_logb`, `ieee_rint`, `ieee_scalb`, `ieee_next_after`) are implemented with runtime helpers that inspect bit patterns, so NaN checks still work under `-Ofast`.

Getting and setting the rounding mode and exception flags reads and writes the hardware control registers (FPCR/FPSR on ARM64, MXCSR on x86_64). GVN and CSE will not merge floating-point operations that depend on the rounding mode across a mode change.

The F2023 `ieee_max`, `ieee_min`, `ieee_max_mag`, `ieee_min_mag` functions and their `_num` variants are implemented.

`IEEE_SUPPORT_UNDERFLOW_CONTROL`, `IEEE_SUPPORT_HALTING`, and `IEEE_SUPPORT_STANDARD` return false, because those features are not supported.

### I/O

- `PRINT` and `WRITE`, formatted and list-directed
- List-directed integer output uses the same field widths as gfortran for each kind
- `READ` from stdin and files
- `OPEN`, `CLOSE`, `INQUIRE`, `REWIND`, `BACKSPACE`, `ENDFILE`, `FLUSH`
- Unformatted (binary) I/O
- Stream I/O
- Non-advancing I/O
- `FORMAT` statements
- `NAMELIST` groups

### Intrinsics

Mathematical: `ABS`, `SQRT`, `EXP`, `LOG`, `LOG10`, `SIN`, `COS`, `TAN`, `ASIN`, `ACOS`, `ATAN`, `ATAN2`, `SINH`, `COSH`, `TANH`, `MOD`, `MODULO`, `SIGN`, `DIM`, `FLOOR`, `CEILING`, `NINT`, `INT`, `REAL`, `DBLE`, `MAX`, `MIN`, `MAXVAL`, `MINVAL`, `SUM`, `PRODUCT`

Array: `SIZE`, `SHAPE`, `LBOUND`, `UBOUND`, `ALLOCATED`, `ASSOCIATED`, `RESHAPE`, `TRANSPOSE`, `MATMUL`, `DOT_PRODUCT`, `PACK`, `UNPACK`, `SPREAD`, `MERGE`, `COUNT`, `ANY`, `ALL`

Character: `LEN`, `LEN_TRIM`, `TRIM`, `ADJUSTL`, `ADJUSTR`, `INDEX`, `SCAN`, `VERIFY`, `CHAR`, `ICHAR`, `ACHAR`, `IACHAR`, `REPEAT`, `NEW_LINE`

Bit: `IAND`, `IOR`, `IEOR`, `NOT`, `ISHFT`, `ISHFTC`, `IBITS`, `IBSET`, `IBCLR`, `BTEST`, `POPCNT`, `POPPAR`, `LEADZ`, `TRAILZ`

System: `SYSTEM_CLOCK`, `CPU_TIME`, `DATE_AND_TIME`, `RANDOM_NUMBER`, `RANDOM_SEED`, `COMMAND_ARGUMENT_COUNT`, `GET_COMMAND_ARGUMENT`, `GET_COMMAND`, `GET_ENVIRONMENT_VARIABLE`

Inquiry: `KIND`, `SELECTED_INT_KIND`, `SELECTED_REAL_KIND`, `HUGE`, `TINY`, `EPSILON`, `PRECISION`, `RANGE`, `DIGITS`, `RADIX`, `MINEXPONENT`, `MAXEXPONENT`

### Optimization levels

| Level | Passes |
|-------|--------|
| `-O0` | None; IR is left as lowered |
| `-O1` | mem2reg, constant folding, DCE, basic CSE, copy propagation, small inlining |
| `-O2` | `-O1` plus LICM, strength reduction, DSE, GVN, SROA, loop store forwarding, jump threading, IPO (constant args, dead args, return propagation) |
| `-Os` | Like `-O2`, but no unrolling and less inlining |
| `-O3` | `-O2` plus aggressive inlining, loop unrolling and interchange, vectorization (NEON on arm64, SSE2 on x86_64) |
| `-Ofast` | `-O3` plus fast-math: reassociation, multiply-add contraction, reciprocals, and the assumption that no NaN or Inf values occur |

On x86_64 the vectorizer only emits SSE2, which every x86_64 CPU has. CI fails if any SSE3 or later instruction, any AVX instruction, or any x87 instruction shows up in the output.

Floating-point multiply-add contraction is off at every level except `-Ofast`. At `-Ofast` it may emit fused instructions where the target has them. Baseline x86_64 (SSE2) has none, so it keeps the multiply and add separate.

A program that gives correct output at `-O0` must give the same output at `-O1`, `-O2`, `-Os`, and `-O3`. `-Ofast` may differ only in ways its fast-math rules allow. The end-to-end test suite checks this at every level.

### Modules

`iso_c_binding` and `iso_fortran_env` are built in. For multi-file builds, the driver scans source files for `MODULE`, `USE`, and `SUBMODULE` statements, sorts the files by dependency, and writes and reads `.amod` files. It accepts gfortran's `-J` and `-I` flags, so fpm can use armfortas as its compiler without changes.

Submodules (F2008) are supported:

- separate module procedure bodies, in both the `module procedure NAME` form and the `module function` / `module subroutine` prefix form
- nested submodule trees
- host association from submodules, including access to the parent's PRIVATE entities
- separate module procedures used as type-bound procedure targets or behind generic interfaces

Submodules are sorted after their parents, so an unordered file list compiles in one call. A mismatch between an interface and its implementation, or a submodule whose parent can't be found, is a compile-time error.

The dependency scanner reads one line at a time. A `MODULE`, `USE`, or `SUBMODULE` statement split across continuation lines is not recognized.

## Not yet supported

- Coarrays
- C descriptors (`CFI_cdesc_t` / `ISO_Fortran_binding.h`). Declarations that would need one, including `BIND(C) CHARACTER(len=*)`, are rejected.
- UCS-4 (`ISO_10646`) characters. `SELECTED_CHAR_KIND` returns 4, but kind-4 character storage and I/O are not implemented.
- Internal READ from a whole character array. This is rejected with an error; read the elements one at a time instead. (Internal WRITE to a character array works, one record per element.)
- Parameterized derived types, beyond what our target projects use

## Architecture

```
armfortas/
├── afs-as/          Standalone assembler (git submodule)
│   └── src/         ARM64 + x86_64 encoding, .s parsers, Mach-O + ELF emission
├── afs-ld/          Standalone linker (git submodule), ELF + Mach-O
├── src/
│   ├── preprocess/  Fortran-aware preprocessor (#ifdef, #include, #define)
│   ├── lexer/       Tokenization, free-form + fixed-form
│   ├── parser/      Recursive descent → AST
│   ├── ast/         AST node definitions
│   ├── sema/        Symbol tables, type system, .amod modules, validation
│   ├── ir/          SSA-form IR with block parameters (no phi nodes)
│   ├── opt/         Optimization passes and pass manager
│   ├── target/      Target identity: arch, OS, libc, object format
│   ├── codegen/
│   │   ├── arm64/   ARM64 isel, linear-scan regalloc, peephole, emission
│   │   └── x86/     x86_64 isel, two-address conversion, linear-scan, emission
│   ├── driver/      CLI, compilation orchestration, linking
│   └── runtime/     Runtime interface: I/O, intrinsics, memory management
├── bencch/          Compiler benchmark and test harness (git submodule)
├── test_programs/   About 880 end-to-end test programs with inline assertions
└── runtime/         libarmfortas_rt source
```

### Design decisions

#### No LLVM or GCC backend

The gfortran bugs we hit are in GCC's backend, and the flang bugs are in how LLVM's frontend lowers Fortran. Building on either would bring those bugs along, so every pass is our own.

#### SSA IR with block parameters

Blocks take typed parameters in place of phi nodes. We find this easier to build, check, and transform. The mem2reg pass promotes stack allocas to SSA values using iterated dominance frontiers (Cytron et al.).

#### Platform ABIs

On ARM64 we follow Apple AAPCS64: the stack is always 16-byte aligned, x18 is never used, x29 and x30 are saved in the prologue, and the frame pointer is always kept. On x86_64 we follow SysV AMD64, including its integer and SSE register classes, red zone, and stack-passed arguments.

#### Host and target are separate

`TargetSpec::host()` in `src/target.rs` is the only code in the workspace that reads `cfg!(target_*)`. Everything else takes a `TargetSpec` value, which is why `--target x86_64-linux-gnu -c` works on any host.

#### Array descriptors

Layout: `{base_addr, elem_size, rank, flags, dims[15]}`. This is part of our ABI and will stay stable across releases.

#### String descriptors

Layout: `{data, len, capacity, flags}`. Assigning to a deferred-length string allocates the new buffer before freeing the old one. This avoids the kind of use-after-free behind gfortran's allocatable string crashes on ARM64.

#### Large arrays on the heap

Arrays over 64KB go on the heap instead of the stack. gfortran corrupts the stack with arrays larger than about 600KB.

#### Polymorphic dispatch

Each type with bound procedures gets one constant vtable, `_afs_vtable_<module>_<type>`. It holds the type tag, a pointer to the parent type's vtable, and the bindings in declaration order with the parent's slots first. A call loads the table, loads the slot, and calls through it. An override takes its parent's slot, so dispatch works in translation units that never saw the type's source, and on elements of polymorphic arrays.

#### afs-as and afs-ld

afs-as is a standalone assembler for ARM64 and x86_64 (a subset of AT&T syntax) that writes Mach-O and ELF. It has no Fortran-specific code. It runs in-process by default, and its output is checked byte-for-byte against the system assembler across the whole test corpus. afs-ld is a standalone linker built the same way. The driver uses the system `ld` unless `AFS_LD=1` is set.

## Testing

```bash
cargo test --workspace                            # all unit + integration tests
cargo test --test run_programs                    # end-to-end at -O0
cargo test --test run_programs -- --nocapture     # verbose output
cargo run -p afs-tests -- run --suite runtime     # bencch runtime suite
cargo run -p afs-tests -- run --suite consistency # reproducibility checks
```

The root test harness is the fast runner. It compiles each `.f90` file in `test_programs/`, runs the binary, and checks assertions written as comments in the source:

| Directive | Checks |
|-----------|--------|
| `! CHECK:` | stdout |
| `! STDERR_CHECK:` | runtime stderr |
| `! EXIT_CODE:` | exact exit status |
| `! XFAIL:` | marks a known open bug |
| `! ERROR_EXPECTED:` | a diagnostic that must be emitted |
| `! ERROR_SPAN:` | exact location of a diagnostic |
| `! ASM_CHECK:` / `! ASM_NOT:` | generated assembly |
| `! IR_CHECK:` / `! IR_NOT:` | generated IR |
| `! FILE_CHECK:` / `! FILE_NOT:` | contents of files written in the sandbox |
| `! FILE_EXISTS:` / `! FILE_MISSING:` | whether a sandbox file exists |
| `! FILE_LINE_COUNT:` | number of lines in a file |
| `! FILE_RERUN_MODE:` | whether a rerun should overwrite or append |
| `! FILE_SET_EXACT:` | the exact set of files the program creates |
| `! REPRO_CHECK:` | asm, object, and run output are reproducible |
| `! OPT_EQ:` | output matches across optimization levels |
| `! PHASE_TRIANGULATE:` | IR, asm, and object are all produced at one level, compile cleanly, and are reproducible |

These comments are the project's main assertion language, and new kinds of assertion should be added to the root harness first.

`bencch` runs the same annotated tests with more structure around them. Use it for optimization-level matrices, differential runs against reference compilers, module graphs, capability-aware execution, and reports. It uses the same annotation syntax as the root harness, and the two should stay in sync.

Every root end-to-end test runs at every optimization level from `-O0` to `-Ofast`. Tests for known bugs, including imported compatibility fixtures, carry an `! XFAIL:` annotation whose reason starts with an ID: either an `XFAIL-NNN` entry from `.docs/audits/xfail-debt.md` or an `X64-O0-NNN` sweep finding. An XFAIL test counts as passing while the bug is open. Once the bug is fixed, the unexpected pass fails CI so the annotation gets removed.

## Targets

- `arm64-macos`: Apple Silicon (M1 to M4), Mach-O, Apple AAPCS64
- `x86_64-linux-gnu` and `x86_64-linux-musl`: ELF, SysV AMD64
- `x86_64-freebsd`: ELF, SysV AMD64

Language standards F77 through F2018 are in scope. Work started from F2018 and extends back to older standards.

The projects we test against:

- fortsh: builds, and passes 3776 of 3776 tests in its POSIX suite
- fpm: builds itself to a byte-identical fixed point
- toml-f, test-drive, and the rest of the list in [PROJECT_CAMPAIGN.md](PROJECT_CAMPAIGN.md)

fortsh is one milestone. The goal is a compiler that handles Fortran in general, including code fortsh doesn't use.
