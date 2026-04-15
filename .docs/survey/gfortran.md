# gfortran Frontend Architecture

Source: gcc/gcc/fortran/ in the GCC tree.

## File Organization
gfortran is a GCC frontend written in C. Key files:

- **parse.cc** — Top-level parser. Calls into decl.cc, match*.cc for specific constructs.
- **scanner.cc** — Lexer. Handles both free-form and fixed-form. The fixed-form whitespace insensitivity logic is here — it's ~3000 lines and notoriously complex.
- **decl.cc** — Declaration parsing and processing.
- **match*.cc** — Pattern matching for statements (matchexpr.cc for expressions, etc.).
- **resolve.cc** — Name resolution and semantic checking. ~17,000 lines. This is where USE association, host association, and generic resolution happen.
- **trans*.cc** — Translation to GCC's GENERIC tree IR. This is the "lowering" phase.
  - **trans-decl.cc** — Declaration lowering
  - **trans-expr.cc** — Expression lowering
  - **trans-stmt.cc** — Statement lowering
  - **trans-array.cc** — Array descriptor and operations lowering (~8000 lines, one of the largest)
  - **trans-intrinsic.cc** — Intrinsic function lowering
  - **trans-io.cc** — I/O statement lowering
- **symbol.cc** — Symbol table management.
- **module.cc** — Module file (.mod) reading and writing.
- **simplify.cc** — Compile-time constant folding for intrinsics.
- **intrinsic.cc** — Intrinsic function table (~13,000 lines defining all ~400 intrinsics).

## Data Flow
```
Source → scanner.cc (tokens) → parse.cc/match*.cc (gfc_code, gfc_expr trees)
→ resolve.cc (semantic checking) → trans*.cc (GENERIC IR) → GCC middle end
```

## Key Data Structures
- `gfc_symbol` — Symbol table entry (name, type, attributes, namespace pointer)
- `gfc_expr` — Expression tree node
- `gfc_code` — Statement/executable code node
- `gfc_typespec` — Type specification (type enum + kind + char length + derived type ref)
- `gfc_array_spec` — Array shape specification (rank, dimension bounds)
- `gfc_namespace` — Scope (contains symbol hash table, USE associations)

## Where the ARM64 Bugs Live
The bugs are primarily in **trans-array.cc** and **trans-expr.cc** — the lowering of array descriptors and string operations to GCC's generic IR. The gfortran frontend parses Fortran correctly; it's the translation to GCC's representation that introduces ARM64-specific issues because:
1. Array descriptor layout assumptions baked into trans-array.cc don't account for ARM64 alignment
2. String descriptor temporaries in trans-expr.cc sometimes reuse stack slots incorrectly on ARM64
3. The callee-saved register convention differs on Apple ARM64 and some save/restore sequences in the generated code clobber descriptor metadata

## Takeaways for ARMFORTAS
- gfortran's **parser** is solid and well-tested — we can trust its behavior as a reference for what valid Fortran looks like
- gfortran's **intrinsic table** (intrinsic.cc) is an excellent reference for all ~400 intrinsics with their signatures
- We should study **trans-array.cc** to understand array descriptor layout, then do it differently (and correctly for ARM64)
- The **resolve.cc** approach to name resolution is battle-tested — we should follow a similar scope-chaining model
