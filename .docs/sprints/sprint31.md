# Sprint 31: Multi-Standard Support & Fixed-Form Codegen

## Prerequisites
Sprint 6 (fixed-form lexer), Sprint 14 (semantic analysis), Sprint 17+ (codegen)

## Goals
Wire up the `--std=` flag to enforce standard conformance throughout the pipeline, and ensure fixed-form source code compiles all the way to binaries (not just lexes). This sprint makes ARMFORTAS a multi-standard compiler.

## Deliverables

### 1. Standard Enum and Feature Gating
```rust
#[derive(Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
enum FortranStandard {
    F77,
    F90,
    F95,
    F2003,
    F2008,
    F2018,
    F2023,
}

struct FeatureGate {
    name: &'static str,
    introduced: FortranStandard,
    deprecated: Option<FortranStandard>,
    removed: Option<FortranStandard>,
}

// Examples:
const ALLOCATABLE_SCALARS: FeatureGate = FeatureGate {
    name: "allocatable scalar variables",
    introduced: F2003,
    deprecated: None,
    removed: None,
};

const ARITHMETIC_IF: FeatureGate = FeatureGate {
    name: "arithmetic IF statement",
    introduced: F77,
    deprecated: Some(F90),
    removed: None,  // still allowed but deprecated
};

const DO_CONCURRENT: FeatureGate = FeatureGate {
    name: "DO CONCURRENT construct",
    introduced: F2008,
    deprecated: None,
    removed: None,
};
```

### 2. Parser Enforcement
When the parser encounters a feature:
```rust
fn check_standard(&self, feature: &FeatureGate, span: Span) {
    if feature.introduced > self.opts.standard {
        self.error(span, format!(
            "{} requires --std=f{} or later (current: --std=f{})",
            feature.name,
            feature.introduced.year(),
            self.opts.standard.year(),
        ));
    }
    if let Some(dep) = feature.deprecated {
        if self.opts.standard >= dep && self.opts.pedantic {
            self.warning(span, format!(
                "{} is deprecated since Fortran {}",
                feature.name, dep.year(),
            ));
        }
    }
}
```

### 3. Semantic Analysis Enforcement
Some features need semantic-level checking:
- F77: `implicit` typing by default, no `implicit none`
- F90: modules, allocatable arrays, derived types
- F95: PURE, ELEMENTAL, FORALL
- F2003: OOP (type extension, CLASS, type-bound procedures), allocatable scalars, `move_alloc`
- F2008: DO CONCURRENT, submodules, BLOCK construct, `error stop`
- F2018: teams, events, IMPORT enhancements
- F2023: conditional expressions, enumerations

### 4. Fixed-Form Through the Pipeline
Sprint 6 built the fixed-form lexer. Now verify fixed-form code works all the way through:

```fortran
C     Classic F77 program
      PROGRAM HELLO
      INTEGER I, N
      N = 10
      DO 10 I = 1, N
         WRITE(*,*) 'Hello', I
   10 CONTINUE
      STOP
      END
```

This must lex → parse → type-check → IR → codegen → assemble → link → run.

### 5. Mixed Source Forms
A project may contain both free-form (`.f90`) and fixed-form (`.f`) files:
```bash
afs legacy.f modern.f90 -o program    # mixed forms
```

Each file is lexed in its appropriate mode. The token stream and AST are form-agnostic.

### 6. Standard-Specific Test Suites
Create test directories per standard:
```
tests/
├── f77/       (fixed-form, implicit typing, GOTO, COMMON, etc.)
├── f90/       (free-form, modules, allocatable arrays)
├── f95/       (pure, elemental, forall)
├── f2003/     (OOP, allocatable scalars)
├── f2008/     (do concurrent, block, submodule)
├── f2018/     (teams, events, import extensions)
└── f2023/     (conditional expressions)
```

### 7. Warning Flags
```
-Wall           All standard warnings
-Wextra         Extra warnings beyond -Wall
-Wpedantic      Warn on non-standard extensions
-Wdeprecated    Warn on deprecated features
-Werror         Treat warnings as errors
-Wno-*          Disable specific warnings
```

### 8. Legacy Feature Support

**Computed GOTO** codegen:
```fortran
      GO TO (100, 200, 300), I
```
→ range check I, then indirect branch through jump table

**Arithmetic IF** codegen:
```fortran
      IF (X) 10, 20, 30
```
→ compare X with 0, three-way branch

**EQUIVALENCE** codegen:
```fortran
      EQUIVALENCE (A(1), B(1))
```
→ both A and B share the same memory (overlapping allocations)

**COMMON blocks** codegen:
```fortran
      COMMON /BLK/ X, Y, Z
```
→ named global section, symbols at fixed offsets

**ENTRY** codegen:
```fortran
      ENTRY ALT_ENTRY(Y)
```
→ secondary entry point label in same function, jumping past initial code

## Testing Strategy

### Per-Standard Compilation
For each test program, compile with the matching `--std=` flag and verify it works. Compile with a lower standard and verify the expected error.

### Fixed-Form End-to-End
Compile classic F77 programs (BLAS routines, simple numerical programs) and run them.

### Mixed-Form Projects
Compile projects with both `.f` and `.f90` files.

### Legacy Feature Tests
Each legacy feature (COMMON, EQUIVALENCE, ENTRY, computed GOTO, arithmetic IF) gets a dedicated test program that compiles, runs, and produces correct output.

### Pedantic Mode Tests
Compile code with deprecated features using `-Wpedantic`, verify warnings emitted. Compile with `-Wpedantic -Werror`, verify it fails.

## Definition of Done
- `--std=f77` through `--std=f2018` enforced in parser and semantic analysis
- `-Wpedantic` warns on deprecated features
- Fixed-form code compiles end-to-end to working binaries
- Mixed-source projects compile
- Legacy features (COMMON, EQUIVALENCE, ENTRY, computed GOTO, arithmetic IF) work
- Per-standard test suites pass
- Warning flags (-Wall, -Werror, etc.) work
- `cargo test` multi-standard tests pass
