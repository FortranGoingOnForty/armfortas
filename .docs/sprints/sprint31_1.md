# Sprint 31.1: --std= Gating Framework

## Context
Fortran spans F77 through F2023 — nearly 50 years of language evolution. Different codebases target different standards. The compiler needs to accept or reject constructs based on the requested standard level, and provide clear diagnostics when a program uses a feature from a later standard.

## Prerequisites
Sprint 31.0 (audit residuals, implicit none, generic interfaces)

## Deliverables

### 1. --std= Flag Plumbing
**Problem:** The driver accepts `--std=f77` through `--std=f2023` but the flag isn't propagated to the parser or sema.

**Solution:**
- Add `FortranStandard` enum: F77, F90, F95, F2003, F2008, F2018, F2023
- Thread the standard through `Options` → parser → sema → validate
- Default: F2018 (the current de facto target)

**Files:** `src/driver/mod.rs`, `src/parser/mod.rs`, `src/sema/resolve.rs`

### 2. Feature Gate Infrastructure
**Problem:** No mechanism to associate a construct with the standard that introduced it.

**Solution:**
- Create a `feature_gate(std_required, span, feature_name)` helper in sema
- When the active standard is older than `std_required`, emit: `error: <feature> requires --std=f2008 or later`
- Annotate existing constructs with their introduction standard

**Gates to implement:**
| Feature | Standard | Location |
|---------|----------|----------|
| DO CONCURRENT | F2008 | parser/stmt.rs |
| BLOCK construct | F2008 | parser/stmt.rs |
| ASSOCIATE construct | F2003 | parser/stmt.rs |
| ERROR STOP | F2008 | parser/stmt.rs |
| SUBMODULE | F2008 | parser/unit.rs |
| IMPURE keyword | F2008 | parser/unit.rs |
| Deferred-length character | F2003 | sema/validate.rs |
| ABSTRACT type | F2003 | parser/decl.rs |
| Class(*) / TYPE(*) | F2018 | parser/decl.rs |
| Allocatable scalars | F2003 | sema/validate.rs |
| ALLOCATE with SOURCE= | F2003 | parser/stmt.rs |
| MOVE_ALLOC | F2003 | sema intrinsics |
| Coarray syntax | F2008 | parser/decl.rs |

### 3. Per-Standard Test Programs
**Solution:** Create test programs that exercise each feature gate:
- `test_programs/std_gate_do_concurrent.f90` with `! ERROR_EXPECTED: requires` when compiled with `--std=f95`
- One test per gated feature, testing both acceptance (at the right standard) and rejection (at an older standard)

### 4. --std=f77 Strict Mode
**Problem:** F77 has no free-form source, no modules, no derived types. `--std=f77` should reject all post-F77 features.

**Solution:** When `--std=f77`:
- Reject free-form source (require .f extension or --fixed-form)
- Reject MODULE, USE, TYPE, CONTAINS, INTERFACE, etc.
- Accept COMMON, EQUIVALENCE, ENTRY, arithmetic IF, computed GOTO, Hollerith

## Definition of Done
- `--std=f95` rejects DO CONCURRENT with clear diagnostic
- `--std=f2018` accepts all currently-implemented features
- `--std=f77` rejects free-form and modules
- ≥10 feature-gate error tests in test_programs/
- No regressions at default standard (F2018)
