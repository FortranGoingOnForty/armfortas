# Sprint 12: Semantic Analysis — Symbol Tables & Scoping

## Prerequisites
Sprint 11 (parser complete)

## Goals
Build the symbol table infrastructure and implement Fortran's scoping rules. This is the foundation of semantic analysis — every later check (type checking, interface validation, module resolution) depends on being able to look up what a name refers to in any given context.

## Deliverables

### 1. Symbol Table Design
```rust
struct SymbolTable {
    scopes: Vec<Scope>,
    current: ScopeId,
}

struct Scope {
    id: ScopeId,
    parent: Option<ScopeId>,
    kind: ScopeKind,
    symbols: HashMap<String, Symbol>,    // case-insensitive lookup
    implicit_rules: ImplicitRules,
    use_associations: Vec<UseAssociation>,
}

enum ScopeKind {
    Global,
    Module(String),
    Submodule(String),
    Program(String),
    Subroutine(String),
    Function(String),
    Block,
    Interface,
    DerivedType(String),
    Forall,
    Associate,
    Critical,
}

struct Symbol {
    name: String,
    kind: SymbolKind,
    type_info: Option<TypeInfo>,
    attrs: SymbolAttrs,
    defined_at: Span,
    scope: ScopeId,
}

enum SymbolKind {
    Variable,
    Parameter,           // named constant
    Function,
    Subroutine,
    Module,
    DerivedType,
    NamedInterface,      // generic interface
    Enumerator,
    Namelist,
    CommonBlock,
    ExternalProc,
    IntrinsicProc,
    ProcedurePointer,
    Label(u64),
}
```

### 2. Fortran Scoping Rules
Fortran has four ways a name becomes accessible in a scope:

**Local declaration**: Declared in the current scope.
```fortran
subroutine foo()
    integer :: x      ! local to foo
end subroutine
```

**Host association**: Inherited from the enclosing scope (internal subprograms see the host's names).
```fortran
subroutine outer()
    integer :: x = 5
contains
    subroutine inner()
        ! x is accessible here via host association
        print *, x
    end subroutine
end subroutine
```

**USE association**: Imported from a module.
```fortran
use my_module, only: foo, bar
```

**IMPORT statement** (F2018): Explicitly imports host entities into interface bodies.
```fortran
interface
    import :: my_type
    subroutine proc(arg)
        type(my_type), intent(in) :: arg
    end subroutine
end interface
```

**Resolution order**: Local > USE association > Host association > Implicit

### 3. Implicit Typing
Fortran's implicit typing rules (unless `implicit none`):
- Names starting with I-N → default integer
- Names starting with A-H, O-Z → default real

`IMPLICIT` statements modify these rules:
```fortran
implicit double precision (a-h, o-z)
implicit integer (i-n)
implicit none                          ! disables implicit typing
implicit none (type)                   ! F2018: disables type inference
implicit none (external)               ! F2018: requires EXTERNAL attribute
implicit none (type, external)         ! both
```

Our symbol table must track implicit rules per scope and apply them when a name is used without declaration (in scopes that allow implicit typing).

### 4. Case Insensitivity
Fortran is case-insensitive: `MyVar`, `myvar`, and `MYVAR` are the same symbol. The symbol table must normalize names (we'll use lowercase internally) while preserving original case for error messages.

### 5. Module Dependency Resolution
When processing a file with `USE module_name`, we need to:
1. Check if the module has already been processed in this compilation
2. If not, find and parse the module's source file (or read a pre-compiled `.amod` file)
3. Populate USE-associated symbols in the current scope

For now (Sprint 12), we handle single-file compilation and modules defined in the same file. Cross-file module resolution comes in Sprint 30.

### 6. First Pass: Symbol Collection
Walk the AST and populate symbol tables:
1. Create scope for each program unit
2. Process USE statements (within same file)
3. Process IMPLICIT statements
4. Process declarations → create symbols
5. Process subprogram definitions → create symbols
6. Handle CONTAINS (create child scopes)
7. Process interface blocks → create symbols

### 7. Accessibility (PUBLIC/PRIVATE)
Module symbols have accessibility:
```fortran
module m
    private           ! default is private
    integer, public :: visible
    integer :: hidden
    public :: also_visible
end module
```

USE association only imports public symbols (unless accessing from a submodule).

## Testing Strategy

### Scope Tests
- Declare a variable, look it up → found
- Look up undeclared variable with implicit typing → implicitly typed
- Look up undeclared variable with `implicit none` → error
- Host association: inner subprogram sees outer's variables
- USE association: using module makes public symbols visible
- Local declaration shadows host association
- Local declaration shadows USE association

### Name Collision Tests
```fortran
use mod1, only: foo          ! foo from mod1
use mod2, only: foo          ! foo from mod2 — error! (ambiguous)
use mod1, only: a => foo     ! rename resolves ambiguity
use mod2, only: b => foo
```

### Accessibility Tests
- Private module members not accessible via USE
- Public members accessible
- Default accessibility (public unless `private` statement)
- Submodule access to private parent members

### fortsh Symbol Resolution
Build symbol tables for fortsh modules. Verify all names resolve correctly (no "undeclared variable" errors for valid code).

## Definition of Done
- Symbol tables correctly represent Fortran scoping hierarchy
- Local, USE, host, and IMPORT association all work
- Implicit typing applies correctly (and implicit none enforces)
- Case-insensitive lookup with original case preserved
- Module public/private accessibility enforced
- Same-file module dependency resolution works
- No false "undeclared" errors on fortsh source
- `cargo test` symbol table tests pass
