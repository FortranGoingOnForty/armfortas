# Audit 02: semantic analysis and module correctness

## Scope and method

Audited tree: `/tmp/armfortas-audit` at `a6ef0b1dd713ff3c07641401f774b9286a30a89d`
(the implementation parent is `23857aa4`). The compiler under test was:

```sh
AFS=/tmp/armfortas-audit/target/release/armfortas
P=/tmp/armfortas-audit-probes
$AFS --version
# armfortas 0.1.0 (x86_64-linux-gnu)
```

Reference diagnostics were checked with GNU Fortran 16.1.1 using
`-std=f2018 -pedantic-errors`. All experiments were ordinary, fixed source
examples compiled locally. No fuzzing, malformed artifact generation, security
inspection, implementation edits, or commits were performed.

The review covered symbol scopes, USE and IMPORT association, explicit
interfaces, assignment and argument type checking, authored modules,
submodules, `.amod` records, dependency ordering, and multi-source object
identity.

## Summary

| ID | Severity | Finding |
|---|---|---|
| A02-01 | High | Ambiguous USE-associated references compile and select the first module |
| A02-02 | Medium | A local declaration illegally shadows a directly USE-associated entity |
| A02-03 | High | IMPORT, including IMPORT, NONE, has no semantic effect |
| A02-04 | High | Intrinsic assignment does not check incompatible declared types |
| A02-05 | High | Explicit-interface calls do not check actual/dummy type compatibility |
| A02-06 | Medium | A rename in a bare USE leaves the remote name accessible |
| A02-07 | Medium | INTRINSIC/NON_INTRINSIC USE nature is parsed and ignored |
| A02-08 | Medium | USE, ONLY targets are not validated against the provider |
| A02-09 | High | `.amod` dependencies erase ONLY filters and re-export excluded names |
| A02-10 | High | A submodule can define a separate module procedure with no ancestor interface |
| A02-11 | High | Separate-module-procedure INTENT mismatches are accepted |
| A02-12 | High | A submodule's IMPLICIT NONE statement is discarded |
| A02-13 | High | Nested submodules do not validate or resolve the immediate parent submodule |
| A02-14 | Medium | Duplicate module program units are merged and accepted |
| A02-15 | Medium | END MODULE names are consumed without comparison |
| A02-16 | Medium | Semicolon-separated USE statements are invisible to multi-file ordering |
| A02-17 | High | Multi-source link mode aliases same-basename source objects |

## Verified discrepancies

### A02-01: ambiguous USE-associated references select the first module

**Source location:** `src/sema/symtab.rs:435-466` returns the first matching
USE association. `src/ir/lower/core.rs:8291-8304` detects a later global
collision only during lowering, warns, and explicitly keeps the first.
`tests/cli_driver.rs:731-789` asserts this success-and-warning behavior.

**Source example:**

```fortran
module mod_a
  implicit none
  integer :: x = 1
end module mod_a
module mod_b
  implicit none
  integer :: x = 2
end module mod_b
program ambiguous_use
  use mod_a
  use mod_b
  implicit none
  print *, x
end program ambiguous_use
```

**Exact command:**

```sh
$AFS -std=f2018 -c "$P/ambiguous_use.f90" \
  -J "$P/runs/afs/ambiguous" -I "$P/runs/afs/ambiguous" \
  -o "$P/runs/afs/ambiguous/ambiguous.o"
```

**Actual result:** exit 0 with
`warning: ambiguous USE import 'x' from both 'mod_a' and 'mod_b'; keeping the first`.

**Intended result:** a compile-time error when `x` is referenced. The two names
identify distinct entities and are ambiguous. GNU Fortran rejects the reference
as ambiguous.

**Consequence:** source order chooses program meaning. Reordering USE statements
or adding an unrelated module can silently change which object is read or
written.

**Confidence:** High.

### A02-02: a local declaration shadows a directly USE-associated entity

**Source location:** `src/sema/symtab.rs:289-305` checks duplicate local symbols
only; it does not check `use_associations`. `src/sema/symtab.rs:429-433` then
gives the local symbol priority. The unit test at
`src/sema/symtab.rs:1410-1434` codifies this as `local_shadows_use`.

**Source example:**

```fortran
module values
  implicit none
  integer :: answer = 42
end module values
program local_use_conflict
  use values
  implicit none
  integer :: answer
  answer = 7
  print *, answer
end program local_use_conflict
```

**Exact command:**

```sh
$AFS -std=f2018 -c "$P/local_use_conflict.f90" \
  -J "$P/runs/afs/local_conflict" -I "$P/runs/afs/local_conflict" \
  -o "$P/runs/afs/local_conflict/local.o"
```

**Actual result:** exit 0 with no diagnostic.

**Intended result:** reject the local declaration because `answer` is already
the local name of a use-associated entity. GNU Fortran reports that the symbol
conflicts with the entity from module `values`.

**Consequence:** an accidental declaration disconnects code from module state
without a diagnostic, and the symbol table's documented lookup priority embeds
non-Fortran shadowing semantics.

**Confidence:** High.

### A02-03: IMPORT controls have no semantic effect

**Source location:** the AST retains imports (`src/ast/unit.rs:20-67`), but all
resolver arms destructure them as `imports: _`, for example
`src/sema/resolve/core.rs:197-247`. Interface bodies are placed under a normal
child scope at `src/sema/resolve/core.rs:547-560`, so ordinary parent lookup
provides unrestricted host association.

**Source example:**

```fortran
module import_host
  implicit none
  integer, parameter :: host_kind = 8
  interface
    function external_value(x) result(r)
      import, none
      integer(host_kind), intent(in) :: x
      integer(host_kind) :: r
    end function external_value
  end interface
end module import_host
```

**Exact command:**

```sh
$AFS -std=f2018 -c "$P/import_none.f90" \
  -J "$P/runs/afs/import_none" -I "$P/runs/afs/import_none" \
  -o "$P/runs/afs/import_none/import.o"
```

**Actual result:** exit 0. `host_kind` is resolved from the enclosing module in
spite of `IMPORT, NONE`.

**Intended result:** reject both `integer(host_kind)` declarations because the
interface body has explicitly disabled import of host entities. GNU Fortran
reports that `host_kind` has not been declared as a constant in the interface.

**Consequence:** explicit interfaces can acquire forbidden host types and kind
values. This can make the caller and separately compiled callee disagree about
procedure characteristics and ABI widths. `IMPORT, ONLY` and selective IMPORT
are equally unenforced by the same implementation gap.

**Confidence:** High.

### A02-04: intrinsic assignment omits incompatible-type checking

**Source location:** `src/sema/validate/core.rs:2743-2750` validates only the
assignment target and pure-procedure effects. It never compares the target and
value types.

**Source example:**

```fortran
program assignment_type
  implicit none
  integer :: value
  value = .true.
  print *, value
end program assignment_type
```

**Exact command:**

```sh
$AFS -std=f2018 -c "$P/assignment_type.f90" \
  -J "$P/runs/afs/assignment" -I "$P/runs/afs/assignment" \
  -o "$P/runs/afs/assignment/assignment.o"
```

**Actual result:** exit 0 with no diagnostic.

**Intended result:** reject conversion from `LOGICAL(4)` to `INTEGER(4)` in
intrinsic assignment. GNU Fortran does so.

**Consequence:** the verified invalid source reaches lowering, where
implementation-specific bit coercions become observable behavior. The validator
has no general assignment-compatibility gate; only special-purpose cases are
checked.

**Confidence:** High.

### A02-05: explicit-interface calls omit actual/dummy type checking

**Source location:** a detailed checker exists at
`src/sema/types.rs:845-950`, but production code never calls it; all references
other than the definition are unit tests in the same file.
`src/sema/validate/core.rs:2931-2954` invokes only
`validate_call_site_intent`, whose body at `src/sema/validate/core.rs:3340-3390`
collects facts and emits no diagnostics.

**Source example:**

```fortran
module call_api
  implicit none
contains
  subroutine take_integer(value)
    integer, intent(in) :: value
    print *, value
  end subroutine take_integer
end module call_api
program call_type
  use call_api
  implicit none
  call take_integer(.true.)
end program call_type
```

**Exact command:**

```sh
$AFS -std=f2018 -c "$P/call_type.f90" \
  -J "$P/runs/afs/call" -I "$P/runs/afs/call" \
  -o "$P/runs/afs/call/call.o"
```

**Actual result:** exit 0 with no diagnostic.

**Intended result:** reject the `LOGICAL(4)` actual for an `INTEGER(4)` dummy.
GNU Fortran reports a type mismatch in argument `value`.

**Consequence:** the verified incompatible actual reaches ABI lowering, so an
explicit interface does not provide its core type-safety property. The
disconnected helper is also the code intended to diagnose arity and keyword
association, leaving duplicated and incomplete call checks elsewhere.

**Confidence:** High.

### A02-06: a bare USE rename leaves the remote name accessible

**Source location:** `src/sema/resolve/use_resolution.rs:78-105` first imports
every public symbol under its original name. Lines 107-117 then add each rename
without suppressing the original association.

**Source example:**

```fortran
module rename_source
  implicit none
  integer, parameter :: original = 19
end module rename_source
program use_rename_original
  use rename_source, local => original
  implicit none
  print *, original
end program use_rename_original
```

**Exact command:**

```sh
$AFS -std=f2018 -c "$P/use_rename_original.f90" \
  -J "$P/runs/afs/rename" -I "$P/runs/afs/rename" \
  -o "$P/runs/afs/rename/rename.o"
```

**Actual result:** exit 0; `original` remains visible.

**Intended result:** only local name `local` denotes the renamed entity in this
scoping unit. GNU Fortran rejects `original` under `IMPLICIT NONE`.

**Consequence:** renames do not isolate names, so supposedly removed remote
names can collide with locals or bind references unexpectedly.

**Confidence:** High.

### A02-07: USE nature is ignored

**Source location:** the parser records `UseNature::Intrinsic` and
`UseNature::NonIntrinsic`, but `process_uses` discards the field as `nature: _`
at `src/sema/resolve/use_resolution.rs:24-35`.

**Source example:**

```fortran
module authored_module
  implicit none
  integer, parameter :: value = 23
end module authored_module
program use_intrinsic_nature
  use, intrinsic :: authored_module
  implicit none
  print *, value
end program use_intrinsic_nature
```

**Exact command:**

```sh
$AFS -std=f2018 -c "$P/use_intrinsic_nature.f90" \
  -J "$P/runs/afs/nature" -I "$P/runs/afs/nature" \
  -o "$P/runs/afs/nature/nature.o"
```

**Actual result:** exit 0 and binding to the authored module.

**Intended result:** reject the USE because `authored_module` is not an
intrinsic module. GNU Fortran reports that no intrinsic module with that name
exists. Conversely, `NON_INTRINSIC` must not silently bind a built-in module.

**Consequence:** source that deliberately selects module nature can bind a
different interface than requested, undermining portability and replacement
module workflows.

**Confidence:** High.

### A02-08: USE, ONLY targets are not validated

**Source location:** `src/sema/resolve/use_resolution.rs:44-75` installs an
association for every ONLY item without checking that the source scope exports
the named entity. There is no corresponding declaration validation.

**Source example:**

```fortran
module only_source
  implicit none
  integer, parameter :: present_name = 1
end module only_source
program use_only_missing
  use only_source, only: absent_name
  implicit none
  print *, 'accepted'
end program use_only_missing
```

**Exact command:**

```sh
$AFS -std=f2018 -c "$P/use_only_missing.f90" \
  -J "$P/runs/afs/only_missing" -I "$P/runs/afs/only_missing" \
  -o "$P/runs/afs/only_missing/only_missing.o"
```

**Actual result:** exit 0 with no diagnostic.

**Intended result:** diagnose at the USE statement that `absent_name` is not
found in `only_source`. GNU Fortran does so even though the bad import is not
later referenced.

**Consequence:** misspelled or stale module contracts remain latent, and builds
can appear valid until a distant use site or configuration enables the name.
Private names in ONLY lists have the same validation gap.

**Confidence:** High.

### A02-09: `.amod` erases ONLY filters on re-export dependencies

**Source location:** `src/sema/amod.rs:247-300` serializes only a deduplicated
`@uses module` edge plus explicit renames. `ModuleInterface` has no per-edge
ONLY set (`src/sema/amod.rs:1518-1531`). On load,
`src/sema/resolve/use_resolution.rs:267-305` reconstructs every dependency as a
bare USE and re-exports every public symbol.

**Source examples:**

```fortran
! base.f90
module base
  implicit none
  integer, parameter :: exported = 7
  integer, parameter :: excluded = 99
end module base
```

```fortran
! facade.f90
module facade
  use base, only: exported
  implicit none
end module facade
```

```fortran
! consumer.f90
program consumer
  use facade
  implicit none
  print *, excluded
end program consumer
```

**Exact commands:**

```sh
cd "$P/runs/afs/amod_only"
$AFS -std=f2018 -c "$P/amod_only/base.f90" \
  -J "$P/runs/afs/amod_only" -I "$P/runs/afs/amod_only" \
  -o "$P/runs/afs/amod_only/base.o"
$AFS -std=f2018 -c "$P/amod_only/facade.f90" \
  -J "$P/runs/afs/amod_only" -I "$P/runs/afs/amod_only" \
  -o "$P/runs/afs/amod_only/facade.o"
$AFS -std=f2018 -c "$P/amod_only/consumer.f90" \
  -J "$P/runs/afs/amod_only" -I "$P/runs/afs/amod_only" \
  -o "$P/runs/afs/amod_only/consumer.o"
```

**Actual result:** all three commands exit 0. `facade.amod` contains only
`@uses base`; loading it exposes both `exported` and `excluded` from
`base.amod`.

**Intended result:** compiling `consumer.f90` must fail because `excluded` was
never accessible in `facade` and therefore cannot be re-exported. GNU Fortran
rejects the consumer and suggests `exported`.

**Consequence:** visibility depends on whether a module came from source or an
`.amod`. Separate compilation widens public APIs, leaks private dependency
choices, changes generic/operator candidate sets, and can silently bind a name
that a same-invocation build rejects.

**Confidence:** High.

### A02-10: missing separate-module-procedure interfaces are accepted

**Source location:** `validate_smp_body` documents that the interface must
exist at `src/sema/validate/core.rs:1137-1142`, but explicitly returns success
when no interface scope is found at `src/sema/validate/core.rs:1186-1197`.

**Source example:**

```fortran
module missing_interface_parent
  implicit none
end module missing_interface_parent
submodule (missing_interface_parent) missing_interface_child
contains
  module subroutine stray()
    print *, 'stray'
  end subroutine stray
end submodule missing_interface_child
```

**Exact command:**

```sh
$AFS -std=f2018 -c "$P/submodule_missing_interface.f90" \
  -J "$P/runs/afs/smp_missing" -I "$P/runs/afs/smp_missing" \
  -o "$P/runs/afs/smp_missing/smp_missing.o"
```

**Actual result:** exit 0 with no diagnostic.

**Intended result:** reject `stray`; a separate module procedure body requires
a corresponding declaration in an ancestor module. GNU Fortran rejects the
submodule because the parent has no module-procedure interface.

**Consequence:** misspelled or orphan implementations compile into symbols that
no valid client interface owns. The error can surface only as an undefined
procedure elsewhere, or the stray symbol can mask another linkage mistake.

**Confidence:** High.

### A02-11: separate-module-procedure INTENT mismatches are accepted

**Source location:** `src/sema/validate/core.rs:1199-1242` compares argument
count, declared type/kind, and rank only. It does not compare INTENT, OPTIONAL,
VALUE, ALLOCATABLE, POINTER, procedure attributes, or result characteristics.

**Source example:**

```fortran
module intent_parent
  implicit none
  interface
    module subroutine update(value)
      integer, intent(in) :: value
    end subroutine update
  end interface
end module intent_parent
submodule (intent_parent) intent_child
contains
  module subroutine update(value)
    integer, intent(out) :: value
    value = 9
  end subroutine update
end submodule intent_child
```

**Exact command:**

```sh
$AFS -std=f2018 -c "$P/submodule_intent_mismatch.f90" \
  -J "$P/runs/afs/smp_intent" -I "$P/runs/afs/smp_intent" \
  -o "$P/runs/afs/smp_intent/smp_intent.o"
```

**Actual result:** exit 0 with no diagnostic.

**Intended result:** reject the body because its dummy characteristics do not
match the ancestor interface. GNU Fortran reports an INTENT mismatch.

**Consequence:** callers are validated and optimized against one contract while
the implementation uses another. Differences involving OPTIONAL, VALUE,
descriptors, or results can also alter the physical ABI.

**Confidence:** High.

### A02-12: submodule IMPLICIT NONE is discarded

**Source location:** `parse_submodule` receives the parsed implicit part as
`_implicit` and drops it at `src/parser/unit.rs:281-300`. The `Submodule` AST
variant has no implicit field. Resolver lines `src/sema/resolve/core.rs:385-425`
therefore never call `process_implicit` for a submodule.

**Source example:**

```fortran
module implicit_parent
  implicit none
  interface
    module subroutine run()
    end subroutine run
  end interface
end module implicit_parent
submodule (implicit_parent) implicit_child
  implicit none
contains
  module subroutine run()
    typo = 1
  end subroutine run
end submodule implicit_child
```

**Exact command:**

```sh
$AFS -std=f2018 -c "$P/submodule_implicit_none.f90" \
  -J "$P/runs/afs/smp_implicit" -I "$P/runs/afs/smp_implicit" \
  -o "$P/runs/afs/smp_implicit/smp_implicit.o"
```

**Actual result:** exit 0; `typo` is implicitly typed.

**Intended result:** reject `typo` as undeclared. GNU Fortran does so.

**Consequence:** the most important typo barrier in submodule implementations
is ineffective. This is especially risky because submodules are commonly used
to separate large implementation bodies from their interfaces.

**Confidence:** High.

### A02-13: nested submodules ignore the immediate parent

**Source location:** the parser stores `(ancestor:parent)` as `parent` plus
`ancestor` at `src/parser/unit.rs:268-297`. Resolution then discards
`ancestor` and loads only `parent` (the ancestor module) at
`src/sema/resolve/core.rs:385-417`. Validation likewise tests only that module
name at `src/sema/validate/core.rs:2342-2367`. `.smod` files are emitted by
`src/driver/mod.rs:1830-1860`, but no reader exists.

**Source examples:**

```fortran
! ancestor.f90
module ancestor
  implicit none
end module ancestor
```

```fortran
! child.f90
submodule (ancestor:no_such_parent) child
end submodule child
```

**Exact commands:**

```sh
cd "$P/runs/afs/nested_parent"
$AFS -std=f2018 -c "$P/submodule_parent/ancestor.f90" \
  -J "$P/runs/afs/nested_parent" -I "$P/runs/afs/nested_parent" \
  -o "$P/runs/afs/nested_parent/ancestor.o"
$AFS -std=f2018 -c "$P/submodule_parent/child.f90" \
  -J "$P/runs/afs/nested_parent" -I "$P/runs/afs/nested_parent" \
  -o "$P/runs/afs/nested_parent/child.o"
```

**Actual result:** both commands exit 0 although `no_such_parent` has never
been defined.

**Intended result:** reject `child.f90` because the immediate parent submodule
does not exist. GNU Fortran looks for `ancestor@no_such_parent.smod` and fails.

**Consequence:** invalid ancestry is accepted, and a valid nested child cannot
reliably receive host association from immediate-parent submodule declarations
across files. The emitted `.smod` record is currently bookkeeping, not an
enforced semantic contract.

**Confidence:** High.

### A02-14: duplicate module program units are merged

**Source location:** `resolve_file` creates a scope for every module without a
uniqueness check at `src/sema/resolve/core.rs:73-78`. Each later module body
calls first-match `find_module_scope` (`src/sema/resolve/core.rs:219-238` and
`src/sema/symtab.rs:852-864`), so duplicate units populate the first scope.

**Source example:**

```fortran
module duplicate_name
  implicit none
  integer, parameter :: first = 1
end module duplicate_name
module duplicate_name
  implicit none
  integer, parameter :: second = 2
end module duplicate_name
```

**Exact command:**

```sh
$AFS -std=f2018 -c "$P/duplicate_module_name.f90" \
  -J "$P/runs/afs/duplicate_module" -I "$P/runs/afs/duplicate_module" \
  -o "$P/runs/afs/duplicate_module/dup.o"
```

**Actual result:** exit 0. The single emitted `duplicate_name.amod` contains
both `@param first` and `@param second`, proving that two program units were
silently merged.

**Intended result:** reject the second module as a duplicate global module
name. GNU Fortran does so.

**Consequence:** invalid source creates an interface that no source module
actually declares. Across files, provider selection can instead depend on
scan order and artifact overwrite order.

**Confidence:** High.

### A02-15: END MODULE names are not checked

**Source location:** `consume_end` accepts and discards any trailing identifier
at `src/parser/stmt.rs:1845-1876`; callers pass only the construct keyword, not
the opening program-unit name.

**Source example:**

```fortran
module declared_name
  implicit none
  integer, parameter :: value = 1
end module different_name
```

**Exact command:**

```sh
$AFS -std=f2018 -c "$P/end_module_name_mismatch.f90" \
  -J "$P/runs/afs/end_name" -I "$P/runs/afs/end_name" \
  -o "$P/runs/afs/end_name/end.o"
```

**Actual result:** exit 0 with no diagnostic.

**Intended result:** when an END MODULE name is present, it must match the
opening module name. GNU Fortran reports that `declared_name` was expected.

**Consequence:** structural copy/paste errors are hidden, weakening diagnostics
for large files and generated source. The shared helper also affects named
programs, subroutines, functions, submodules, and named constructs.

**Confidence:** High.

### A02-16: semicolon-separated USE is missed by dependency ordering

**Source location:** `scan_file` examines only the beginning of each physical
line (`src/driver/dep_scan.rs:31-130`) and does not split legal semicolon
statements. `compile_multi` trusts that scan for order at
`src/driver/mod.rs:2398-2405`.

**Source examples:**

```fortran
! provider.f90
module provider
  implicit none
  integer, parameter :: answer = 42
end module provider
```

```fortran
! consumer.f90
module consumer; use provider
  implicit none
contains
  integer function get_answer()
    get_answer = answer
  end function get_answer
end module consumer
```

```fortran
! main.f90
program ordering_main
  use consumer
  implicit none
  if (get_answer() /= 42) error stop 1
  print *, 'ok'
end program ordering_main
```

**Exact failing command:**

```sh
cd "$P/runs/afs/ordering"
$AFS -std=f2018 "$P/ordering/consumer.f90" "$P/ordering/provider.f90" \
  "$P/ordering/main.f90" -J "$P/runs/afs/ordering" \
  -I "$P/runs/afs/ordering" -o "$P/runs/afs/ordering/app"
```

**Actual result:** exit 1 while compiling `consumer.f90`:
`module 'provider' not found`. The scanner records the module definition at the
start of the line but never sees the following USE.

**Intended result:** topologically compile provider, consumer, and main
regardless of input order. The control command with provider listed first
compiled and ran, printing `ok`.

**Consequence:** a valid free-form spelling defeats the advertised unordered
multi-source build and makes success depend on command-line order.

**Confidence:** High.

### A02-17: same-basename sources alias one temporary object

**Source location:** multi-source link mode names each temporary object solely
from `src.file_stem()` at `src/driver/mod.rs:2416-2430`. Unlike the single-file
temporary path logic, it does not include a source-path identity hash.

**Source examples:**

```fortran
! a/unit.f90
module first_module
  implicit none
  integer, parameter :: first_value = 10
end module first_module
```

```fortran
! b/unit.f90
module second_module
  implicit none
  integer, parameter :: second_value = 20
end module second_module
```

```fortran
! main.f90
program same_stem_main
  use first_module
  use second_module
  implicit none
  if (first_value + second_value /= 30) error stop 1
  print *, 'ok'
end program same_stem_main
```

**Exact command:**

```sh
cd "$P/runs/afs/same_stem"
$AFS -std=f2018 "$P/same_stem/a/unit.f90" "$P/same_stem/b/unit.f90" \
  "$P/same_stem/main.f90" -J "$P/runs/afs/same_stem" \
  -I "$P/runs/afs/same_stem" -o "$P/runs/afs/same_stem/app"
```

**Actual result:** exit 2. Both source files compile to the same
`/tmp/afs_multi_<pid>/unit.o`; the second overwrites the first and the linker is
given that same object twice, reporting a multiple definition of
`afs_mod_second_module_second_value`.

**Intended result:** each source has a distinct object identity and the program
links and prints `ok`. The equivalent GNU Fortran command does so.

**Consequence:** ordinary projects that organize repeated filenames in
different directories cannot use the multi-source driver. Depending on link
shape, the result is duplicate definitions, missing symbols, or the wrong
object occupying a link-list position.

**Confidence:** High.

## Test-coverage concerns

1. `tests/cli_driver.rs:731-789` currently requires ambiguous USE to succeed
   with a warning, and `src/sema/symtab.rs:1410-1434` requires a local entity to
   shadow a direct USE import. These tests preserve the first two language
   discrepancies instead of detecting them.
2. `sema::types::check_arguments` has extensive unit tests but no production
   caller. There are no CLI-negative tests for incompatible intrinsic
   assignment or a nongeneric explicit-interface call with wrong type, rank,
   arity, keyword, or nondefinable OUT/INOUT actual.
3. IMPORT coverage proves parsing and positive BIND(C) use only. No test checks
   default interface isolation, `IMPORT, NONE`, `IMPORT, ONLY`, or an import of
   a missing host entity.
4. USE tests cover successful ONLY lists and renames but not a missing/private
   ONLY target, the remote-name suppression rule for a bare rename, or a module
   whose requested INTRINSIC/NON_INTRINSIC nature is wrong.
5. Cross-file `.amod` tests cover bare transitive re-export and explicit
   renames. They do not place `USE dep, ONLY: x` in a facade and then consume
   the facade with bare USE, which is the record-fidelity case in A02-09.
6. Separate-module-procedure negative fixtures cover arity and declared
   type/rank. They do not cover a missing ancestor declaration or matching of
   INTENT, OPTIONAL, VALUE, POINTER, ALLOCATABLE, PURE/ELEMENTAL, result name,
   or result characteristics.
7. Positive submodule tests contain `IMPLICIT NONE`, but none introduces an
   undeclared name to prove that the statement survived parsing and applies to
   descendant procedure bodies.
8. Nested-submodule dependency tests validate only the scanner's synthetic
   graph. There is no semantic test that reads the immediate parent's `.smod`,
   rejects an unknown immediate parent, or host-associates an immediate-parent
   submodule entity across files.
9. Module parser tests do not cover mismatched END names or duplicate module
   program-unit names.
10. Dependency-scan tests use one statement per physical line. No test covers
    semicolon-separated MODULE/USE statements. Generated multi-file chains are
    otherwise mostly compiled in already-correct order.
11. Temporary-path tests cover concurrent single-file outputs, but no
    multi-source link test uses two source paths with the same file stem.

## Verification limits

This was a focused semantic/module audit, not a full regression run. The local
examples above were compiled with armfortas and GNU Fortran; the report did not
modify or add tests. Backend/runtime behavior was not evaluated beyond the one
successful ordering control and the GNU same-basename control because each
finding is already established at compile or link time.
