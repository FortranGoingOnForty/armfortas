# Sprint 0: Scaffolding & Learning

## Prerequisites
None — this is where it all begins.

## Goals
Stand up the project structure, clone reference material, and do an initial survey of existing compilers so we understand the landscape before writing code.

## Deliverables

### 1. Cargo Workspace
Set up a Rust workspace with two crates:
- `armfortas` — the compiler (binary crate)
- `afs-as` — the standalone assembler (library + binary crate, git submodule)

```
armfortas/
├── Cargo.toml          (workspace root)
├── afs-as/             (git submodule)
│   ├── Cargo.toml
│   └── src/
│       ├── lib.rs
│       └── main.rs
├── src/
│   ├── main.rs
│   ├── preprocess/
│   ├── lexer/
│   ├── parser/
│   ├── ast/
│   ├── sema/
│   ├── ir/
│   ├── opt/
│   ├── codegen/
│   ├── runtime/
│   └── driver/
├── tests/
├── docs/
├── .docs/
└── .refs/
```

Each subdirectory gets a `mod.rs` with a placeholder so the structure compiles from day one.

### 2. Reference Clones (.refs/)
Clone into `.refs/` (gitignored):
- **GCC/gfortran source** — `gcc-mirror/gcc` (the `gcc/fortran/` subtree is the frontend)
- **flang-new source** — `llvm/llvm-project` (the `flang/` subtree)
- **LFortran source** — for reference on Fortran parser design
- **Fortran stdlib** — `fortran-lang/stdlib` (real-world Fortran 2018 code for testing)
- **fortran-lang/fpm** — Fortran package manager (more real-world test code)
- **Fortran test suites** — any publicly available conformance suites
- **ARM64 architecture reference** — ARM Architecture Reference Manual (ARMv8-A) PDF if available, or links

### 3. Initial Survey Notes
Produce `.docs/survey/` with notes on:
- How gfortran's frontend is structured (files, passes, data structures)
- How flang-new's frontend is structured (Fortran::parser, Fortran::semantics, Fortran::lower)
- ARM64 instruction set overview (instruction formats, encoding patterns)
- Mach-O object file format overview
- Apple AAPCS64 calling convention specifics

### 4. CI Skeleton
- `cargo build` succeeds
- `cargo test` runs (with placeholder tests)
- `cargo clippy` clean

## Testing Strategy
- `cargo build --workspace` compiles cleanly
- `cargo test --workspace` passes (placeholder tests)
- All refs cloned and accessible
- Survey notes complete

## Definition of Done
- Workspace structure exists and compiles
- `afs-as` submodule initialized
- All reference repos cloned into `.refs/`
- Survey notes document the architecture of gfortran, flang-new, and ARM64 ISA
- We have a clear mental model of the landscape before writing compiler code
