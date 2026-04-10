# armfortas

A Fortran compiler for ARM64. No borrowed frontends, no LLVM, no GCC. Every stage from lexing to machine code is ours.

## Why

[fortsh](https://github.com/FortranGoingOnForty/fortsh) is a ~57,000-line Fortran 2018 shell. When we went to compile it on Apple Silicon we found out that Fortran on ARM64 is, charitably, underserved:

- **gfortran** has at least seven confirmed critical bugs on macOS ARM64, most of them involving allocatable strings — the exact feature fortsh leans on hardest. The bugs live in GCC's backend and the GCC team's queue for ARM64 Fortran is not short.
- **flang-new (LLVM)** works around the gfortran bugs but introduces its own, particularly around C interop and derived type layout. It also requires a separate Homebrew installation and the binary is called `flang-new` for reasons.
- Both compilers are millions of lines of code we don't own. When something breaks in a corner of AArch64 AAPCS64 that nobody expected a Fortran program to reach, "read the source and fix it" is not a realistic option.

The solution was to write a compiler that runs on the machine we actually have, that we can fix when it breaks.

## Status

Active development. The full pipeline — preprocessor through Mach-O object emission — is working. Real Fortran programs compile and run.

```
Pipeline: Source → Preprocessor → Lexer → Parser → AST →
          Sema → SSA IR → Optimizations → ARM64 Codegen →
          afs-as → Mach-O .o → ld → Binary
```

The system linker (`ld`) is the only component we delegate. Everything else is ours.

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
```

## What Works

### Language coverage (F77 through F2018)

- Free-form and fixed-form source
- All numeric types: `integer`, `real`, `double precision`, `logical`, `character`
- Complex arithmetic (storage and arithmetic operations; some intrinsics pending)
- Derived types with component access, type extension (`EXTENDS`), and type-bound procedures with `PASS`/`NOPASS`
- `FINAL` procedures
- `SELECT TYPE` with `TYPE IS` and `CLASS IS` guards
- `ALLOCATABLE` scalars and arrays, including allocatable character strings
- `POINTER` and `TARGET` attributes
- `OPTIONAL` arguments with `PRESENT()` intrinsic
- Full array sections and whole-array expressions
- `WHERE` / `FORALL` constructs
- `DO`, `DO WHILE`, `DO CONCURRENT` with locality specs
- `SELECT CASE` on integer, character, and logical
- `ASSOCIATE` and `BLOCK` constructs
- `GOTO` and labeled statements
- `EQUIVALENCE` and `COMMON` blocks
- `NAMELIST` I/O
- `SAVE` attribute with correct static storage
- `VALUE` attribute for pass-by-value (`BIND(C)`)
- `RECURSIVE` functions and subroutines
- Generic procedures and interfaces
- Operator overloading
- Statement functions
- Arithmetic IF
- `STOP` / `ERROR STOP` with stop codes

### C interoperability (`iso_c_binding`)

Full `iso_c_binding` module: kind parameters (`C_INT`, `C_DOUBLE`, `C_CHAR`, etc.), `C_PTR`, `C_NULL_PTR`, `C_LOC`, `C_FUNPTR`, `BIND(C)` procedures with correct ABI including `VALUE` argument dispatch.

### I/O

- `PRINT` and `WRITE` with format strings and list-directed I/O
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

### Optimization passes

| Level | Passes |
|-------|--------|
| `-O0` | None (preserve IR exactly) |
| `-O1` | mem2reg, constant folding, local CSE, constant propagation, DCE |
| `-O2` | `-O1` + strength reduction, LICM |
| `-O3` | Same as `-O2` (vectorization and IPO deferred — see below) |
| `-Ofast` | `-O3` + fast-math reassociation for float add/sub constant chains |

Correctness invariant: every program that produces correct output at `-O0` must produce identical output at `-O1`, `-O2`, `-O3`. This is enforced by the end-to-end test suite at every level.

### Modules

`iso_c_binding` and `iso_fortran_env` are built-in and always available. Authored modules compile correctly. Multi-file module dependency resolution and `.amod` files are in progress (Sprint 30).

## What Doesn't Work Yet

**In progress or deferred:**

- Complex number intrinsics (`REAL()`, `AIMAG()`, `CONJG()`, `CMPLX()`) — storage works, intrinsic calls don't
- Stack frames larger than ~32KB — prologue/epilogue currently broken above that threshold; practically affects very large local arrays
- `-O3` vectorization (NEON/SIMD) — accepted and correct but runs `-O2` passes
- Function inlining — not yet implemented at any level
- Multi-file compilation and `.amod` module files — Sprint 30
- Loop unrolling, GVN, SROA, dead store elimination — later optimizer sprints
- `ieee_arithmetic`, `ieee_exceptions` modules
- Coarray Fortran
- Submodules
- Vtable-based polymorphic dispatch through `CLASS` variables

## Architecture

```
armfortas/
├── afs-as/          Standalone ARM64 assembler (git submodule)
│   └── src/         Instruction encoding, .s parser, Mach-O emission
├── src/
│   ├── preprocess/  Fortran-aware preprocessor (#ifdef, #include, #define)
│   ├── lexer/       Tokenization — free-form + fixed-form
│   ├── parser/      Recursive descent → AST
│   ├── ast/         AST node definitions
│   ├── sema/        Symbol tables, type system, validation
│   ├── ir/          SSA-form IR with block parameters (no phi nodes)
│   ├── opt/         Optimization passes and pass manager
│   ├── codegen/     ARM64 instruction selection, linear scan register allocation
│   ├── driver/      CLI, compilation orchestration
│   └── runtime/     libarmfortas_rt — I/O, intrinsics, memory management
├── bencch/          Compiler benchmark and test harness (git submodule)
├── test_programs/   ~110 end-to-end test programs with CHECK annotations
└── runtime/         Runtime library source
```

### Key design decisions

**No LLVM.** gfortran's bugs are in GCC's backend. flang's bugs are in LLVM's frontend lowering. Using either as a backend would mean inheriting the bugs we're trying to escape. We own every pass.

**SSA IR with block parameters.** Instead of phi nodes, blocks carry typed parameters. Cleaner to construct, easier to verify, simpler to transform. The mem2reg pass promotes stack allocas to SSA values using iterated dominance frontiers (Cytron et al.).

**Apple AAPCS64 strictly.** 16-byte stack alignment always, x18 reserved, x29/x30 saved in prologue, frame pointer maintained. We've been bitten by every one of these constraints and handle them correctly.

**Array descriptors.** `{base_addr, elem_size, rank, flags, dims[15]}`. Our ABI — stable across releases.

**String descriptors.** `{data, len, capacity, flags}`. Deferred-length assignment always allocates new storage before freeing old. This prevents the use-after-free that causes gfortran's ARM64 allocatable string crashes.

**Large arrays on heap.** Stack threshold at 64KB. Prevents the stack corruption gfortran exhibits with arrays over ~600KB.

**afs-as** is the standalone ARM64 assembler. It knows nothing about Fortran — clean API boundary. It can be used independently to assemble ARM64 `.s` files.

## Testing

```bash
cargo test --workspace                            # all unit + integration tests
cargo test --test run_programs                    # end-to-end at -O0
cargo test --test run_programs -- --nocapture     # verbose output
cargo run -p afs-tests -- run --suite runtime     # bencch runtime suite
cargo run -p afs-tests -- run --suite consistency # reproducibility checks
```

<<<<<<< HEAD
The root `armfortas` harness is the fast, armfortas-first runner. It compiles
each `.f90` file in `test_programs/`, runs the binary, and evaluates
source-embedded assertions such as:

- `! CHECK:` for stdout
- `! STDERR_CHECK:` for runtime stderr
- `! EXIT_CODE:` for exact runtime exit status
- `! XFAIL:` for known open bugs
- `! ERROR_EXPECTED:` for diagnostics that must be emitted
- `! ERROR_SPAN:` for exact diagnostic location
- `! ASM_CHECK:` / `! ASM_NOT:` for assembly shape
- `! FILE_CHECK:` / `! FILE_NOT:` for sandbox file side effects
- `! FILE_EXISTS:` / `! FILE_MISSING:` for explicit sandbox presence or absence
- `! FILE_LINE_COUNT:` for structural file-shape assertions
- `! FILE_RERUN_MODE:` for explicit overwrite vs append intent across reruns
- `! FILE_SET_EXACT:` for exact runtime side-effect file sets
- `! REPRO_CHECK:` for per-test asm/object/run reproducibility
- `! OPT_EQ:` for explicit cross-opt invariants
- `! PHASE_TRIANGULATE:` for same-opt IR/ASM/object availability, compile-cleanliness, and compile-only reproducibility oracles
- `! IR_CHECK:` / `! IR_NOT:` for IR shape

Those source comments are the canonical leaf-assertion language for the
project. The root harness is where new annotation ideas should land first.

`bencch` is the structured matrix/reporting/differential runner around that
same testing language. It is best for:

- opt matrices
- differential/reference runs
- module graphs
- capability-aware execution
- reports and bundles

The two surfaces are meant to converge on syntax and expectations, not drift
into separate testing dialects.

All root end-to-end tests run at every optimization level (`-O0` through
`-Ofast`). Programs with known bugs carry `! XFAIL:` annotations that reference
the audit finding — they count as passing until the bug is fixed, at which
point CI catches the unexpected success.

## Target

- **Architecture**: ARM64 (AArch64), Apple Silicon (M1/M2/M3/M4)
- **OS**: macOS (Mach-O, Apple AAPCS64)
- **Standard**: F77 through F2018, building inward from F2018
- **Goal**: Compile fortsh — a 57,000-line Fortran 2018 shell — correctly on Apple Silicon

Compiling fortsh is a milestone, not the finish line. A complete compiler handles code fortsh never exercises.

## Relationship to fortsh

This compiler exists because fortsh exists. Building a non-trivial Fortran program on ARM64 and discovering that neither available compiler handles it reliably was the motivation. The goal is a compiler we can fix when it breaks, running on the hardware we actually use.
