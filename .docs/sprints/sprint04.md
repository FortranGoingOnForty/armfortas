# Sprint 4: Preprocessor

## Prerequisites
Sprint 0 (workspace exists)

Note: This sprint can run in parallel with Sprints 1-3 (assembler work). The preprocessor is independent of the assembler.

## Goals
Implement a Fortran-aware C-style preprocessor. This is a text-to-text transformation that runs before lexing. Fortran compilers conventionally support cpp-style directives — fortsh uses them, and real-world Fortran code depends on them.

## Deliverables

### 1. Directive Parsing
Recognize and process lines beginning with `#`:

```fortran
#define MAX_SIZE 1024
#define SQUARE(x) ((x) * (x))
#ifdef __APPLE__
  ! Apple-specific code
#elif defined(__linux__)
  ! Linux-specific code
#else
  ! Fallback
#endif
#include "config.h"
#undef MAX_SIZE
#error "Unsupported platform"
#warning "Deprecated feature"
#line 42 "original.f90"
```

### 2. Macro Expansion
**Object-like macros:**
```fortran
#define PI 3.14159265358979d0
real(8) :: area = PI * r * r
```

**Function-like macros:**
```fortran
#define MAX(a, b) merge((a), (b), (a) > (b))
x = MAX(foo, bar)
```

**Variadic macros:**
```fortran
#define DEBUG_PRINT(fmt, ...) write(0, fmt) __VA_ARGS__
```

**Stringification and token pasting:**
```fortran
#define STR(x) #x
#define CONCAT(a, b) a ## b
```

**Recursive expansion** — macros can expand to text containing other macros. Must detect and prevent infinite recursion.

### 3. Conditional Compilation
Full conditional evaluation:
- `#if EXPR` — evaluate constant integer expressions
- `#ifdef NAME` / `#ifndef NAME`
- `#elif EXPR`
- `#else`
- `#endif`
- `defined(NAME)` operator within `#if` expressions
- Arithmetic in conditions: `#if MAX_SIZE > 512 && defined(USE_LARGE)`
- Nested conditionals (arbitrary depth)

### 4. File Inclusion
- `#include "file.h"` — search relative to current file, then include paths
- `#include <file.h>` — search include paths only
- Include path management (`-I` flag from CLI)
- Guard against infinite include recursion
- Track source locations through includes for error reporting

### 5. Predefined Macros
Our compiler defines:
```
__ARMFORTAS__          1
__ARMFORTAS_MAJOR__    0
__ARMFORTAS_MINOR__    1
__aarch64__            1
__APPLE__              1  (on macOS)
__arm64__              1
__FILE__               "current_file.f90"
__LINE__               42
__DATE__               "Apr  3 2026"
__TIME__               "20:30:00"
```

### 6. Fortran-Aware Behavior
The preprocessor must understand enough Fortran to not break:
- Don't expand macros inside Fortran string literals (`'hello'` or `"hello"`)
- Don't expand macros inside Fortran comments (`! comment`)
- Handle Fortran continuation lines (`&` at end of line) — the preprocessor sees these before the lexer
- In fixed-form: `C` or `*` in column 1 is a comment, not a preprocessor directive
- Lines starting with `#` in column 1 are preprocessor directives even in fixed-form

## Testing Strategy

### Unit Tests
- Expand individual object-like macros
- Expand function-like macros with various argument patterns
- Evaluate conditional expressions
- Test nested `#if`/`#elif`/`#else`/`#endif`
- Test `#include` with mock file system

### Integration Tests
- Preprocess Fortran files that use cpp directives
- Compare output with system `cpp -traditional-cpp` (the `-traditional-cpp` flag makes cpp Fortran-friendly)
- Preprocess fortsh source files that use `#ifdef`

### Edge Cases
- Macro expanding to another macro
- Macro that references itself (must not infinite loop)
- Empty `#define`
- `#if 0` blocks (all content skipped)
- Multiline macro definitions with `\` continuation
- `#include` inside `#if 0` block (must not try to open file)

## Key Technical Notes

### Why Not Use System cpp?
- Different cpp implementations have different behaviors with Fortran
- cpp may mangle Fortran string syntax
- We need exact control over predefined macros
- Bespoke philosophy: we own this

### Implementation Structure
```rust
pub struct Preprocessor {
    defines: HashMap<String, MacroDef>,
    include_paths: Vec<PathBuf>,
    file_stack: Vec<SourceFile>,  // for nested includes
    condition_stack: Vec<CondState>,  // for nested #if
}

pub fn preprocess(source: &str, filename: &str, config: &PreprocConfig) -> Result<PreprocOutput>

pub struct PreprocOutput {
    text: String,
    source_map: SourceMap,  // maps output lines → original file:line
}
```

The `SourceMap` is critical — when the lexer/parser report errors, they need to point to the original source location, not the preprocessed output.

## Definition of Done
- All cpp directives listed above work correctly
- Fortran string literals and comments are not corrupted by macro expansion
- Source locations are correctly tracked through includes and macro expansions
- Preprocessor output for fortsh source files matches expected behavior
- `cargo test` preprocessor tests pass
