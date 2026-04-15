# Sprint 30: Module System & Multi-File Compilation

## Prerequisites
Sprint 12 (symbol tables), Sprint 14 (semantic analysis), Sprint 16 (IR)

## Goals
Implement the module file format (.amod), module dependency resolution, compilation ordering for multi-file projects, and incremental compilation. Fortran modules create inter-file dependencies — a file that `USE`s a module must be compiled after the module. This sprint makes ARMFORTAS handle real multi-file projects.

## Deliverables

### 1. Module File Format (.amod)
Our own format — human-inspectable, versioned, and complete.

```
# ARMFORTAS Module File
# Version: 1
# Module: my_module
# Source: src/my_module.f90
# Compiled: 2026-04-03T20:00:00Z
# Checksum: sha256:abc123...

@version 1
@module my_module

@public
  @parameter MAX_SIZE : integer(4) = 1024
  @parameter PI : real(8) = 3.14159265358979d0

  @type container
    @component count : integer(4)
    @component data : real(8), allocatable, dimension(:)
    @typebound area => compute_area
    @typebound get_count, nopass
  @endtype

  @subroutine process(x, n, verbose)
    @arg x : real(8), intent(inout), dimension(:)
    @arg n : integer(4), intent(in)
    @arg verbose : logical(4), intent(in), optional
  @endsubroutine

  @function compute(x) : real(8)
    @arg x : real(8), intent(in)
  @endfunction

  @interface sort
    @specific sort_int(a) : void
      @arg a : integer(4), intent(inout), dimension(:)
    @specific sort_real(a) : void
      @arg a : real(8), intent(inout), dimension(:)
  @endinterface

@private
  @subroutine internal_helper(...)
    ...
  @endsubroutine

@uses
  other_module : only(some_type, some_func)
```

### 2. Module Compilation
When the compiler encounters a `module` definition:
1. Compile the module normally (AST → sema → IR → codegen → object file)
2. Write a `.amod` file containing:
   - Public type definitions (complete layout info)
   - Public procedure interfaces (argument types, attributes)
   - Public named constants (values)
   - Public generic interfaces
   - Private names (just names, for disambiguation)
   - Dependencies (which modules this module USEs)

### 3. Module Consumption
When the compiler encounters `USE module_name`:
1. Search for `module_name.amod` in:
   - Current directory
   - Directories specified by `-I` flags
   - A standard module directory (for iso_c_binding, iso_fortran_env, etc.)
2. Parse the `.amod` file
3. Import public symbols into the current scope
4. Apply ONLY and rename lists

### 4. Dependency Resolution
Given a set of source files, determine compilation order:

```rust
fn resolve_compilation_order(files: &[SourceFile]) -> Result<Vec<&SourceFile>> {
    // 1. Quick-scan each file to find MODULE and USE statements
    //    (lexer-level scan, no full parse needed)
    // 2. Build dependency graph: module → files that define it, file → modules it uses
    // 3. Topological sort
    // 4. Error on cycles (circular module dependencies are illegal in Fortran)
}
```

### 5. Multi-File Compilation
```bash
# Compile individual files
afs -c module_a.f90                    # produces module_a.o, module_a.amod
afs -c module_b.f90 -I.               # finds module_a.amod
afs -c main.f90 -I.                   # finds module_a.amod, module_b.amod
afs module_a.o module_b.o main.o -o program

# Or all at once (compiler resolves order):
afs module_a.f90 module_b.f90 main.f90 -o program
```

When given multiple source files, the driver:
1. Scans for dependencies
2. Determines compilation order
3. Compiles in order, writing `.amod` files to a temp directory
4. Links all object files

### 6. Incremental Compilation
When recompiling:
1. Check if source file changed (timestamp or hash)
2. Check if any `.amod` file it depends on changed
3. If neither changed, skip compilation (reuse existing .o)
4. If module interface changed, recompile all dependents

```rust
struct CompilationCache {
    entries: HashMap<PathBuf, CacheEntry>,
}

struct CacheEntry {
    source_hash: [u8; 32],
    dep_hashes: HashMap<String, [u8; 32]>,  // module name → amod hash
    object_path: PathBuf,
    amod_path: Option<PathBuf>,
}
```

### 7. Built-in Modules
These modules are always available without `.amod` files:
- `iso_c_binding` (Sprint 27)
- `iso_fortran_env` (unit numbers, type kinds, compiler info)
- `ieee_arithmetic` (IEEE floating-point control)
- `ieee_exceptions`
- `ieee_features`

```rust
fn is_intrinsic_module(name: &str) -> bool {
    matches!(name, "iso_c_binding" | "iso_fortran_env" | 
             "ieee_arithmetic" | "ieee_exceptions" | "ieee_features")
}
```

### 8. iso_fortran_env Module
```fortran
use iso_fortran_env
! Constants:
! input_unit = 5 (stdin), output_unit = 6 (stdout), error_unit = 0 (stderr)
! iostat_end, iostat_eor
! int8, int16, int32, int64
! real32, real64, real128
! character_kinds = [1]
! compiler_version(), compiler_options() — inquiry functions
```

## Testing Strategy

### Module Round-Trip
Write a module, compile (produces .amod), compile a program that USEs it, run.

### Dependency Order Tests
Multiple files with complex dependency graphs:
```
A uses B, C
B uses D
C uses D
D uses nothing
```
Verify compilation order: D, then B and C (either order), then A.

### Incremental Tests
- Compile everything, verify .amod files created
- Modify one source file, recompile — verify only affected files recompile
- Modify a module interface — verify dependents recompile
- Modify a module's private section — verify dependents do NOT recompile

### Cycle Detection
```
A uses B
B uses A    ! error: circular dependency
```
Verify clear error message.

### fortsh Module Graph
fortsh has 55 .f90 files with module dependencies. Compile all of them with automatic dependency resolution. This is the ultimate test of the module system.

## Definition of Done
- `.amod` file format defined and implemented (read + write)
- Module compilation produces correct `.amod` files
- `USE` statement finds and loads `.amod` files
- `-I` flag for module search paths works
- Multi-file compilation with automatic dependency ordering works
- Incremental compilation skips unchanged files
- Built-in modules (iso_c_binding, iso_fortran_env) available
- Circular dependency detected with clear error
- fortsh module graph resolves correctly
- `cargo test` module system tests pass
