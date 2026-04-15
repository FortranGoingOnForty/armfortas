# ARMFORTAS

Bespoke ARM64 Fortran compiler written in Rust. No LLVM, no borrowed frontends, no external compiler infrastructure. Every stage of the pipeline — from lexing to machine code emission — is ours.

## Build & Test

```bash
cargo build --workspace          # build compiler + assembler
cargo test --workspace           # run all tests
cargo clippy --workspace         # lint
cargo build -p afs-as            # build assembler only
cargo test -p afs-as             # test assembler only
```

Once the compiler is functional:
```bash
afs hello.f90 -o hello           # compile Fortran
afs -c module.f90                # compile to object
afs -S hello.f90                 # emit assembly
afs-as input.s -o output.o       # standalone assembler
```

## Project Structure

```
armfortas/                       Cargo workspace root
├── afs-as/                      git submodule: standalone ARM64 assembler
│   ├── src/                     instruction encoding, .s parser, Mach-O emission
│   └── Cargo.toml
├── src/
│   ├── preprocess/              Fortran-aware preprocessor (#ifdef, #include, #define)
│   ├── lexer/                   tokenization (free-form + fixed-form)
│   ├── parser/                  recursive descent → AST
│   ├── ast/                     AST node definitions, visitor/walker traits
│   ├── sema/                    semantic analysis, type system, module resolution
│   ├── ir/                      SSA-form IR definition + AST→IR lowering
│   ├── opt/                     optimization passes (constant folding → NEON vectorization)
│   ├── codegen/                 ARM64 instruction selection, register allocation
│   ├── driver/                  CLI, compilation orchestration, --std= dispatch
│   └── runtime/                 libarmfortas_rt (I/O, intrinsics, memory management)
├── tests/
│   ├── lexer/
│   ├── parser/
│   ├── sema/
│   ├── codegen/
│   ├── integration/             end-to-end: .f90 → binary → run → check output
│   └── fortsh/                  ultimate integration test
├── docs/                        generated documentation (tracked)
├── .docs/                       planning documents (gitignored)
├── .refs/                       reference compilers and Fortran resources (gitignored)
└── Cargo.toml
```

## Design Philosophy

- **Bespoke**: We write every component. No parser generators, no LLVM, no cranelift. When something breaks, we read our code and fix it.
- **Zero compiler-infrastructure crates**: Rust standard library is our foundation. No `lalrpop`, `inkwell`, `cranelift`, `logos`, or similar.
- **Test-first, docs-first**: Tests and documentation produced concurrently with code, never after.
- **Total control**: The motivation is gfortran/flang bugs on ARM64 that live in code we can't fix. We own every line.

## Architecture

Pipeline: Source → Preprocessor → Lexer → Parser → AST → Semantic Analysis → IR (SSA) → Optimization → ARM64 Codegen → Assembler (afs-as) → Mach-O .o → System Linker (ld) → Binary.

The system linker is the only thing we delegate. Everything else is ours.

### Key subsystems
- **afs-as**: Standalone ARM64 assembler. Encodes instructions, parses .s files, emits Mach-O objects. Knows nothing about Fortran — clean API boundary.
- **libarmfortas_rt**: Runtime library (Rust → static .a). I/O, memory management, string ops, array intrinsics, system intrinsics. Linked into every binary.
- **Module files (.amod)**: Our own human-inspectable module format. Clean break from gfortran .mod / flang formats.

## Target

- **Architecture**: ARM64 (AArch64), Apple Silicon (M1/M2/M3/M4)
- **OS**: macOS (Mach-O object format, Apple AAPCS64 calling convention)
- **Standards**: --std=f77 through --std=f2023, building from F2018 inward
- **Goal**: A complete, production-quality Fortran compiler for ARM64 that rivals gfortran and flang. Not a toy, not a subset — the full language.

## Coding Conventions

- Rust, idiomatic. Use enums for AST/IR nodes. Exhaustive pattern matching everywhere.
- `unsafe` only where required (machine code emission in assembler). Minimize and isolate unsafe blocks.
- Tests alongside code. Integration tests in `tests/`. Every bug fix gets a regression test.
- Our tests aim to be as impressive as the compiler. Devise a well thought ought harness and runner system for catching all the edge and corner cases.
- Commit often with terse imperative messages. No coauthorship lines. No sprint references.
- Per-file, per-chunk commits. No monoliths.
- If you find yourself about to cut corners, stop yourself and review the options with me to discuss.
- Avoid rushing through sprints to get to an audit. Take your time on the hard work to get it right the first time.
- Always opt for the robust solution, unless the simple first pass is the robust solution.
- If you find yourself saying things like, the simple solution is <this>, stop yourself and ask if a compiler should use the simple solution or if it should dig deeper. The simple solution might be correct, but we want to be sure this is not a toy compiler. 
- If you find yourself unsure, reference the cloned reference implementations in .refs
- Run long running test jobs judiciously. Think about any greps/filters you may need beforehand so we don't lose 10s of minutes to waiting on tests to finish.

## Audits

After each sprint, run a brutally honest audit. The auditor should:
- Assume nothing works until proven otherwise. Test every claim.
- Treat "placeholder" and "stub" as synonyms for "broken." If code returns a wrong value silently, that's critical.
- Check against the Fortran standard, not just "does it compile." Wrong results are worse than crashes.
- Don't soften findings. "Major" means "will produce wrong answers in real code." "Critical" means "silently corrupts results."
- No deferred items unless they genuinely require work from a later sprint. If it can be fixed now, fix it now.
- The audit is not a formality. It's the last line of defense before bad code gets merged.

## Key Technical Decisions

- **No LLVM**: gfortran bugs are in GCC's backend; flang bugs are in LLVM's frontend lowering. We own the whole stack.
- **SSA IR with block parameters**: Instead of phi nodes. Inspired by MLIR/cranelift but ours.
- **Linear scan register allocation**: Simpler than graph coloring, good enough, fast to compile.
- **Array descriptors**: `{base_addr, elem_size, rank, flags, dims[15]}`. Our ABI — stable once committed.
- **String descriptors**: `{data, len, capacity, flags}`. Deferred-length assignment always allocates new before freeing old (prevents use-after-free that kills gfortran).
- **Large arrays on heap**: Stack threshold at 64KB. Prevents gfortran's stack corruption with 600KB+ arrays.
- **Apple ARM64 specifics**: 16-byte stack alignment always, x18 reserved (never allocate), frame pointer (x29) always maintained, x29/x30 saved in prologue.

## Completeness Philosophy

Every Fortran feature from F77 through F2018 is in scope. When implementing a feature:
- Implement it fully, not just the subset that fortsh or any single codebase needs.
- Complex numbers, WHERE/FORALL, array sections, GOTO, EQUIVALENCE, COMMON — all required.
- Intrinsics must be comprehensive, not cherry-picked. If the standard defines it, we support it.
- Don't defer features with "fortsh doesn't use this." Other Fortran programs do.
- The standard is the spec. gfortran behavior is a useful reference, not gospel.
- Don't modify tests that reveal real bugs to suit incorrect armfortas behavior. This is lazy.

## The fortsh Milestone

fortsh (~/Documents/GithubOrgs/FortranGoingOnForty/fortsh) is a ~57K-line Fortran 2018 shell. It uses:
- iso_c_binding extensively (3 C interop files)
- Allocatable strings (gfortran's #1 ARM64 failure)
- Derived types (shell_state_t, command_t, pipeline_t)
- 55 modules with dependency chains
- Recursive descent parsing, 50+ builtins, 8800-line readline

Compiling fortsh is a milestone, not the finish line. A complete compiler must handle code fortsh never exercises.
