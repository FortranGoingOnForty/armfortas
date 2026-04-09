# ARMFORTAS

A bespoke ARM64 Fortran compiler. No borrowed frontends, no LLVM, no external compiler infrastructure. Every stage of the pipeline — from lexing to machine code emission — is ours.

## Motivation

gfortran and flang-new both have critical, unfixable bugs on Apple Silicon (ARM64 macOS). The fortsh project (~57K lines of Fortran 2018) requires extensive C workarounds to compile on this platform:

- **gfortran ARM64 bugs**: stack corruption with large arrays, allocatable string corruption, intent(out) crashes, flush() heap corruption, substring slice crashes, empty string assignment corruption, deferred-length character loss, automatic finalization crashes
- **flang-new ARM64 bugs**: string length bugs, file descriptor caching, 127-character command line limit

These bugs live deep in lowering and codegen — code we can't fix, can't diagnose, and can't work around cleanly. ARMFORTAS eliminates this dependency entirely.

## Design Philosophy

**Bespoke.** We write every component. No parser generators, no LLVM, no cranelift, no borrowed frontends. When something breaks, we read our code and fix it. Total control over every stage of compilation.

**Test-first, docs-first.** Tests and documentation are produced concurrently with code, not after.

**Commit often.** Terse, imperative messages. Per-file, per-chunk. No monolithic commits.

## Testing Posture

`armfortas` now has two serious testing surfaces:

- the in-tree armfortas harness in `tests/` and `test_programs/`
- the structured `bencch/` runner and suite corpus

The project posture is now explicit:

- the root harness is the armfortas-first creative lab
- source comments are the canonical leaf-assertion language
- `bencch` is the structured matrix/reporting/differential runner around that
  same language

We are no longer optimizing for a generic compiler bench as the primary vision.
We are optimizing for the most interesting and effective full-pipeline testing
system for `armfortas`, with `bencch` serving that mission.

The follow-through roadmap for this work lives under `.docs/testing/`. It runs
in parallel with `.docs/sprints/`, which remain the implementation roadmap for
the compiler itself.

## Definition of Done

Produce a working fortsh binary on Apple Silicon with zero macOS/ARM-specific workarounds. The fortsh codebase at `~/Documents/GithubOrgs/FortranGoingOnForty/fortsh` is the ultimate acceptance test.

## Implementation Language: Rust

Rust is the implementation language for the compiler, runtime, and assembler. Rationale:

- **Algebraic types** — AST nodes, IR nodes, and tokens model naturally as Rust enums with exhaustive pattern matching. The Rust compiler catches unhandled cases at build time.
- **Ownership model** — Compiler passes shuffle complex tree structures through transformations. Use-after-free and dangling references are caught at compile time rather than manifesting as mysterious codegen bugs.
- **Performance** — No GC pauses. C-level speed for compilation.
- **`unsafe` when needed** — Machine code emission in the assembler requires raw byte manipulation. Rust permits this in controlled blocks while keeping the rest of the codebase safe.
- **Testing built in** — `cargo test`, integration test directories, and property-based testing align with our test-first approach.
- **Industry precedent** — rustc, swc, oxc, ruff, and rust-analyzer demonstrate that Rust is the modern standard for compiler and language tooling.

Minimal external crate dependencies. Zero compiler-infrastructure crates. The Rust standard library is our foundation; everything else we build.

## Architecture

### Compiler Pipeline

```
Source (.f90/.f)
    │
    ▼
Preprocessor          (#ifdef, #include, #define — our own, not cpp)
    │
    ▼
Lexer                 (free-form and fixed-form, continuation lines, token stream)
    │
    ▼
Parser                (recursive descent, no parser generators)
    │
    ▼
AST                   (typed abstract syntax tree)
    │
    ▼
Semantic Analysis     (type checking, module resolution, interface validation, standard conformance)
    │
    ▼
IR                    (typed SSA-form intermediate representation, Fortran-aware)
    │
    ▼
Optimization          (constant folding, DCE, inlining, array access patterns, Fortran-specific opts)
    │
    ▼
ARM64 Codegen         (instruction selection, register allocation, stack frame layout)
    │
    ▼
Assembler (afs-as)    (ARM64 machine code → Mach-O object files)
    │
    ▼
System Linker (ld)    (linking — the one thing we delegate to the OS)
    │
    ▼
Binary
```

### Project Structure

```
armfortas/
├── afs-as/              ← git submodule: standalone ARM64 assembler
│   ├── src/             (Mach-O emission, ARM64 instruction encoding, .s file parsing)
│   ├── Cargo.toml
│   └── tests/
├── src/
│   ├── preprocess/      (Fortran-aware preprocessor)
│   ├── lexer/           (tokenization, free-form + fixed-form)
│   ├── parser/          (recursive descent parser → AST)
│   ├── ast/             (AST node definitions, visitor/walker traits)
│   ├── sema/            (semantic analysis, type system, module resolution)
│   ├── ir/              (SSA-form IR definition, construction from AST)
│   ├── opt/             (optimization passes over IR)
│   ├── codegen/         (ARM64 instruction selection, register allocation)
│   ├── driver/          (CLI, compilation orchestration, --std= flag dispatch)
│   └── runtime/         (libarmfortas_rt — I/O, intrinsics, memory management)
├── tests/
│   ├── lexer/
│   ├── parser/
│   ├── sema/
│   ├── codegen/
│   ├── integration/     (end-to-end: .f90 → binary → run → check output)
│   └── fortsh/          (the ultimate integration test)
├── docs/
│   ├── overview.md      (this file)
│   └── sprints/         (sprint planning documents)
├── Cargo.toml
└── .refs/               (reference compilers and Fortran resources, gitignored)
```

### The Assembler: afs-as (Git Submodule)

A standalone ARM64 assembler for macOS, usable independently or as a library:

- **Standalone mode**: `afs-as input.s -o output.o` — assembles raw ARM64 assembly into Mach-O object files, like GNU `as`
- **Library mode**: The compiler calls it programmatically to emit machine code without an intermediate text step

The assembler knows nothing about Fortran. The compiler talks to it through a clean Rust API. This separation means the assembler is our first deliverable and can be tested independently against known-good assembly files.

### Multi-Standard Support

```
--std=f77       fixed-form, implicit typing, no modules
--std=f90       free-form, modules, allocatable arrays
--std=f95       pure/elemental, forall
--std=f2003     OOP, type-bound procedures, allocatable components
--std=f2008     coarrays, submodules, do concurrent
--std=f2018     teams, events, iso_c_binding extensions  ← fortsh target
--std=f2023     latest standard
```

Standard conformance is enforced at the parser and semantic analysis layers. Each language feature is gated by the selected standard. Warnings are emitted for extensions and deprecated features.

We build from F2018 inward (since fortsh needs it), not from F77 upward.

### Module System

We define our own `.amod` (ARMFORTAS module) file format. Clean break from gfortran's `.mod` and flang's formats. Our format will be:

- Human-inspectable (not a binary blob)
- Versioned (so module format changes don't silently break builds)
- Containing type signatures, interface blocks, and public symbol information

### Runtime Library (libarmfortas_rt)

Written in Rust, compiled to a static library (`libarmfortas_rt.a`) linked into every produced binary. Provides:

- **I/O subsystem**: `print`, `write`, `read`, `open`, `close`, `inquire`, formatted/unformatted/list-directed I/O
- **Memory management**: `allocate`, `deallocate`, array descriptor management
- **String operations**: `trim`, `adjustl`, `adjustr`, `index`, `scan`, `verify`, `repeat`
- **Array intrinsics**: `matmul`, `reshape`, `pack`, `unpack`, `spread`, `transpose`, `cshift`, `eoshift`
- **Math (complex cases)**: `sin`, `cos`, `exp`, `log` edge cases (simple cases inline to ARM64 FPU instructions)
- **System**: `system_clock`, `cpu_time`, `date_and_time`, `get_command_argument`

Simple intrinsics (`abs`, `max`, `min`, `iand`, `ior`, `ishft`, `btest`, `len`, `size`, `allocated`) are inlined directly as ARM64 instructions during codegen. The runtime is only for operations that require actual function calls.

### Intrinsics Strategy

All ~400 standard intrinsic procedures are implemented. The choice between inlining and runtime call is a codegen optimization decision:

- **Inlined**: arithmetic (`abs`, `max`, `min`, `mod`, `sign`), type conversion (`int`, `real`, `dble`), bit operations (1:1 ARM64 instruction mapping), simple queries (`len`, `size`, `kind`, `allocated`)
- **Runtime calls**: string manipulation, array reshaping/packing, I/O, complex math, system interfaces

### CLI Interface

Invocable as `armfortas` or `afs`. Supports standard compiler CLI paradigms:

```
afs hello.f90 -o hello              # compile and link
afs -c module.f90                   # compile to object file
afs -S hello.f90                    # emit assembly
afs -E hello.f90                    # preprocess only
afs --std=f2018 -O2 -Wall prog.f90  # standard, optimization, warnings
afs-as input.s -o output.o          # standalone assembler
```

### What We Own vs. What We Delegate

**We own (bespoke):**
- Preprocessor
- Lexer
- Parser
- AST
- Semantic analysis
- IR and optimization passes
- ARM64 code generator
- ARM64 assembler (Mach-O object emission)
- Runtime library
- Module file format
- CLI driver

**We delegate (not Fortran-specific):**
- System linker (`ld` on macOS) — Apple's linker knows about dyld, code signing, and Mach-O details that change with every macOS release. Even GCC delegates linking.
- Rust standard library — our foundation, not a dependency

## Key Technical Challenges

1. **Fortran's grammar** — Whitespace insensitivity in fixed-form, ambiguous syntax (is `A(I)` an array access or function call?), implicit typing. Our parser must handle all of this correctly across standards.

2. **Allocatable string handling** — This is gfortran's #1 failure mode on ARM64. Our descriptor layout, copy semantics, and deallocation must be bulletproof.

3. **ARM64 calling convention** — Apple's AAPCS64 variant has specific requirements for stack alignment (16-byte), argument passing, and return values that differ from Linux ARM64.

4. **Fortran I/O** — The I/O subsystem is effectively a small database engine (direct access, sequential, stream, formatted, unformatted, list-directed, namelist). This is one of the largest runtime components.

5. **Stack frame correctness** — gfortran's stack corruption with large arrays is a frame layout bug. We must correctly spill large local arrays to heap and maintain proper frame pointer chains for recursion.

6. **iso_c_binding** — fortsh uses this extensively. Our C interop must match Apple's ARM64 ABI exactly for struct layout, argument passing, and return conventions.
