# flang-new (LLVM Flang) Architecture

Source: llvm/flang/ in the LLVM monorepo.

## File Organization
flang-new is written in C++ and builds on LLVM infrastructure. Key components:

### Frontend (flang/lib/)
- **Parser/** — Fortran parser. Unlike gfortran, flang uses a two-phase approach:
  - **Prescan** (prescan.cpp) — Handles fixed-form/free-form, continuation lines, tokenization. Converts source to a "cooked" stream.
  - **Parsing** (parsing.cpp, grammar.h) — Recursive descent parser using C++ templates for grammar rules. Extremely template-heavy approach.
  - The AST is called "parse tree" and is defined via template metaprogramming in **parse-tree.h** (~5000 lines of C++ type definitions).
- **Semantics/** — Semantic analysis. Key files:
  - **resolve-names.cpp** — Name resolution (~8000 lines)
  - **check-declarations.cpp** — Declaration validation
  - **expression.cpp** — Expression analysis and type checking (~5000 lines)
  - **semantics.cpp** — Orchestrator
  - **type.cpp** — Type representation and checking
- **Evaluate/** — Compile-time evaluation of constant expressions.
- **Lower/** — Lowering from Fortran semantics to FIR (Fortran IR, built on MLIR).
  - **bridge.cpp** — Main lowering bridge
  - **ConvertExpr.cpp** — Expression lowering
  - **IO.cpp** — I/O lowering

### Intermediate Representation
- **FIR (Fortran IR)** — Built on MLIR (Multi-Level IR), LLVM's extensible IR framework.
  - Defines Fortran-specific operations: fir.alloca, fir.load, fir.store, fir.box (descriptors), fir.call
  - Gradually lowered through multiple MLIR dialects → LLVM IR → machine code

### Runtime (flang-rt/)
- **runtime/** — Fortran runtime library written in C++.
  - **io.cpp** — I/O subsystem
  - **allocatable.cpp** — Allocatable variable management
  - **character.cpp** — String operations
  - **descriptor.cpp** — Array descriptor operations
  - **intrinsics.cpp** — Runtime intrinsic implementations

## Data Flow
```
Source → Prescan (cooked chars) → Parser (parse tree) → Semantics (typed parse tree)
→ Lower (FIR/MLIR) → MLIR passes → LLVM IR → LLVM backend → machine code
```

## Where the ARM64 Bugs Live
flang-new's bugs are in **Lower/** and **runtime/**:
1. String descriptor handling in the lowering bridge doesn't properly handle deferred-length reallocation paths on ARM64
2. The runtime's character.cpp has buffer management issues that manifest only on ARM64 due to different stack alignment
3. File descriptor caching in the I/O runtime causes issues on macOS where fd lifecycle differs from Linux

## Takeaways for ARMFORTAS
- flang's **prescan** approach (normalize source before parsing) is smart — we should do something similar
- The **FIR** concept (domain-specific IR) validates our approach of a Fortran-aware SSA IR
- flang's **template-heavy parser** is elegant but incredibly complex to read — we'll use plain recursive descent
- flang's **runtime** library structure is a good reference for what the runtime needs, even though we'll implement ours in Rust
- The **MLIR dialect** approach of gradual lowering through multiple IR levels is interesting but overkill for us — we'll go AST → single IR → ARM64
