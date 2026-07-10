# Audit 03: Parsed-program to IR lowering

## Scope and method

This review examined the lowering path from parsed Fortran statements and
expressions into `src/ir`, together with the IR verifier and focused tests. The
review concentrated on semantic preservation, ownership and cleanup of
temporaries, type consistency, control-flow exits, and deterministic printed
IR.

All behavioral probes were ordinary Fortran source files compiled locally from
`/tmp/armfortas-audit`. Probe sources were kept under
`/tmp/armfortas-audit-cases`; no repository implementation, test, or submodule
files were changed. Existing reports under `.docs/audits` were not inspected.

Baseline verification:

```sh
cd /tmp/armfortas-audit
cargo build
cargo test --lib ir::
```

The focused IR test run passed 108 tests with no failures. That passing result
does not exercise the discrepancies below.

## Confirmed discrepancies

### 1. Allocatable derived local is finalized after its components and allocation are destroyed

**Source locations**

- `src/ir/lower/core.rs:26536-26570` emits implicit cleanup for an allocatable
  derived local in this order: deallocate derived components, deallocate the
  outer descriptor, then invoke final procedures.
- `src/ir/lower/core.rs:26413-26453` contains a separate helper that loads the
  allocation and walks live storage before finalization, but the scope cleanup
  path does not use it.
- `src/ir/lower/core.rs:26313-26361` emits final-procedure calls against the
  address supplied by the caller.

**Example** (`/tmp/armfortas-audit-cases/final_order.f90`)

```fortran
module final_order_m
  implicit none
  type :: box
    integer, allocatable :: payload
  contains
    final :: finish
  end type
contains
  subroutine finish(x)
    type(box), intent(inout) :: x
    if (allocated(x%payload)) then
      print *, 'final payload', x%payload
    else
      print *, 'final missing'
    end if
  end subroutine

  subroutine exercise()
    type(box), allocatable :: x
    allocate(x)
    allocate(x%payload)
    x%payload = 7
  end subroutine
end module

program p
  use final_order_m
  call exercise()
end program
```

**Commands**

```sh
cd /tmp/armfortas-audit
./target/debug/armfortas -O0 --emit-ir -o /tmp/a03-final-order.ir \
  /tmp/armfortas-audit-cases/final_order.f90
./target/debug/armfortas -O0 -o /tmp/a03-final-order \
  /tmp/armfortas-audit-cases/final_order.f90
/tmp/a03-final-order
```

**Actual result**

The program prints:

```text
 final missing
```

The end of the lowered `exercise` function has the destructive operations
before the finalizer:

```text
call @afs_derived_final_order_m_box_dealloc_desc(%70, %68)
call @afs_deallocate_array(%0, %68)
call @afs_modproc_final_order_m_finish(%0)
ret void
```

The final call also receives `%0`, the outer allocation descriptor address,
after that descriptor has been deallocated, rather than the address of a live
`box` object.

**Intended result**

The final subroutine must run while `x` and `x%payload` are still live, and it
must receive the allocated object's storage. The example should print:

```text
 final payload           7
```

Only after the finalizer returns should implicit component and outer-storage
cleanup occur.

**Consequence**

Finalizers of allocatable derived locals cannot reliably inspect their object,
observe allocated components, or perform user-defined release work. Depending
on descriptor contents, they can instead observe a deallocated object or read
descriptor bytes as if they were object storage.

**Confidence:** High. The runtime result and emitted call order directly expose
the defect; local `gfortran` compilation of the same source prints the intended
payload value.

### 2. Rank-specific FINAL receives inline array storage instead of an array descriptor

**Source locations**

- `src/sema/type_layout.rs:80-90` records final procedures only as
  `Vec<String>`, discarding the final dummy's rank and callable ABI.
- `src/ir/lower/core.rs:26313-26361` iterates all recorded final-procedure names
  and passes one unadapted address to each.
- `src/ir/lower/core.rs:26557-26570` invokes that helper for derived locals
  without constructing a descriptor for an array entity.
- `src/ir/verify.rs:24-35` and `src/ir/verify.rs:157-191` verify functions in
  isolation and do not compare call arguments with module-level callee
  signatures.

**Example** (`/tmp/armfortas-audit-cases/derived_rank_final.f90`)

```fortran
module derived_rank_final_m
  implicit none
  integer :: final_count = 0
  type :: counted
    integer :: id = 0
  contains
    final :: finish_rank_one
  end type
contains
  subroutine finish_rank_one(values)
    type(counted), intent(inout) :: values(:)
    final_count = final_count + size(values)
  end subroutine

  subroutine exercise()
    type(counted) :: values(3)
  end subroutine
end module

program p
  use derived_rank_final_m
  call exercise()
  print *, final_count
end program
```

**Commands**

```sh
cd /tmp/armfortas-audit
./target/debug/armfortas -O0 --emit-ir -o /tmp/a03-rank-final.ir \
  /tmp/armfortas-audit-cases/derived_rank_final.f90
./target/debug/armfortas -O0 -o /tmp/a03-rank-final \
  /tmp/armfortas-audit-cases/derived_rank_final.f90
/tmp/a03-rank-final
gfortran -O0 -o /tmp/a03-rank-final-gfortran \
  /tmp/armfortas-audit-cases/derived_rank_final.f90
/tmp/a03-rank-final-gfortran
```

**Actual result**

Armfortas prints `0`; the local reference compiler prints `3`. The IR defines
the final procedure with a descriptor-shaped parameter:

```text
func @afs_modproc_derived_rank_final_m_finish_rank_one(%0: ptr<[i8 x 384]>)
```

The caller instead allocates inline object storage and passes it directly:

```text
%0 = alloca [[i8 x 4] x 3]
...
call @afs_modproc_derived_rank_final_m_finish_rank_one(%0)
```

The module verifier accepts this call despite the incompatible pointer pointee
types and missing descriptor construction.

**Intended result**

Lowering must select the final procedure whose dummy rank matches the entity
being finalized and pass a valid rank-one descriptor with extent 3. The
program should print `3`.

**Consequence**

Rank-specific finalization reads arbitrary inline object bytes as descriptor
metadata. Finalization is skipped in observable behavior here and may produce
invalid extents, out-of-bounds accesses, or crashes for other object layouts.
The absent interprocedural call check lets malformed IR pass verification.

**Confidence:** High. The incompatible caller/callee IR types, zero result, and
reference result agree.

### 3. EXIT from a named ASSOCIATE construct is silently ignored

**Source locations**

- `src/parser/stmt.rs:1212-1216` preserves the `ASSOCIATE` construct name.
- `src/ir/lower/stmt.rs:7520-7563` matches `Stmt::Associate` with `..`, ignores
  that name, and creates no construct-exit target.
- `src/ir/lower/stmt.rs:5963-5970` lowers `EXIT` only when it finds a loop or a
  registered construct target; otherwise it emits no branch.
- `src/ir/lower/stmt.rs:7467-7472` registers such a target for named `BLOCK`,
  showing the missing corresponding handling for `ASSOCIATE`.

**Example** (`/tmp/armfortas-audit-cases/associate_exit.f90`)

```fortran
program p
  implicit none
  integer :: x
  x = 0
scope: associate (y => x)
  y = 1
  exit scope
  y = 99
end associate scope
  print *, x
end program
```

**Commands**

```sh
cd /tmp/armfortas-audit
./target/debug/armfortas -O0 --emit-ir -o /tmp/a03-associate-exit.ir \
  /tmp/armfortas-audit-cases/associate_exit.f90
./target/debug/armfortas -O0 -o /tmp/a03-associate-exit \
  /tmp/armfortas-audit-cases/associate_exit.f90
/tmp/a03-associate-exit
```

**Actual result**

The program prints `99`. The emitted IR contains both the store of 1 and the
subsequent store of 99 in one uninterrupted path; `EXIT scope` produces no
branch.

**Intended result**

`EXIT scope` must branch to the continuation after `end associate scope`, so
the second assignment is unreachable and the program prints `1`.

**Consequence**

Valid named control flow changes program results. Any statements after the
ignored exit execute unexpectedly, including side effects and invalid work that
the source explicitly bypasses.

**Confidence:** High. The parser/lowering disconnect and generated straight-line
IR exactly account for the observed result.

### 4. GOTO leaving a BLOCK bypasses the block cleanup path

**Source locations**

- `src/ir/lower/stmt.rs:7467-7505` puts `BLOCK` finalization and implicit
  deallocation in a dedicated cleanup block reached by fallthrough or a named
  `EXIT` target.
- `src/ir/lower/stmt.rs:7577-7580` lowers `GOTO` as a direct branch to the label
  block without unwinding active construct scopes.

**Example** (`/tmp/armfortas-audit-cases/goto_block_cleanup.f90`)

```fortran
module goto_block_cleanup_m
  implicit none
  integer :: final_count = 0
  type :: counted
  contains
    final :: finish
  end type
contains
  subroutine finish(x)
    type(counted), intent(inout) :: x
    final_count = final_count + 1
  end subroutine
end module

program p
  use goto_block_cleanup_m
  implicit none
outer: block
    type(counted) :: x
    goto 100
  end block outer
100 continue
  print *, final_count
end program
```

**Commands**

```sh
cd /tmp/armfortas-audit
./target/debug/armfortas -O0 --emit-ir -o /tmp/a03-goto-block.ir \
  /tmp/armfortas-audit-cases/goto_block_cleanup.f90
./target/debug/armfortas -O0 -o /tmp/a03-goto-block \
  /tmp/armfortas-audit-cases/goto_block_cleanup.f90
/tmp/a03-goto-block
gfortran -O0 -o /tmp/a03-goto-block-gfortran \
  /tmp/armfortas-audit-cases/goto_block_cleanup.f90
/tmp/a03-goto-block-gfortran
```

**Actual result**

Armfortas prints `0`; the local reference compiler prints `1`. Armfortas IR
branches directly to the label target:

```text
br label_100_1()
```

No finalizer call occurs on that edge.

**Intended result**

Leaving the `BLOCK` must run its scope-exit actions before control reaches label
100. `finish` must execute once, and the program must print `1`.

**Consequence**

Legal unstructured exits from a block skip FINAL side effects and leak any
allocatable locals or components owned by the block. The same direct-edge
architecture puts other non-fallthrough exits at risk unless each is manually
routed through cleanup.

**Confidence:** High. The direct IR edge omits the only generated cleanup path,
and both the counter and reference result demonstrate the semantic difference.

### 5. Nonallocatable derived local does not clean up its allocatable components

**Source locations**

- `src/ir/lower/core.rs:26455-26493` decides whether a local needs implicit
  cleanup from the local's own `allocatable`, character, and final-procedure
  metadata. It does not inspect allocatable components of an otherwise ordinary
  derived local and returns early.
- `src/ir/lower/core.rs:53040-53172` can generate recursive derived-storage
  component deallocation, but the normal-local scope-exit path does not invoke
  it in this case.
- `src/ir/lower/alloc.rs:1159-1189` registers the enclosing scalar object as
  nonallocatable, which triggers the early-return case.

**Example** (`/tmp/armfortas-audit-cases/local_component_cleanup.f90`)

```fortran
module local_component_cleanup_m
  implicit none
  type :: box
    character(:), allocatable :: payload
  end type
contains
  subroutine exercise()
    type(box) :: x
    x%payload = 'owned temporary storage'
  end subroutine
end module

program p
  use local_component_cleanup_m
  call exercise()
end program
```

**Commands**

```sh
cd /tmp/armfortas-audit
./target/debug/armfortas -O0 --emit-ir -o /tmp/a03-local-component.ir \
  /tmp/armfortas-audit-cases/local_component_cleanup.f90
sed -n '/func @afs_modproc_local_component_cleanup_m_exercise/,/^  }/p' \
  /tmp/a03-local-component.ir
```

**Actual result**

The lowered `exercise` function initializes the object and allocates/assigns
the deferred character component, then returns directly:

```text
%0 = alloca [i8 x 32]
call @afs_derived_local_component_cleanup_m_box_init(%2)
...
call @afs_assign_char_deferred(%7, %8, %9)
ret void
```

There is no call to the generated box storage-deallocation helper and no
`afs_dealloc_string` in `exercise`. A component-deallocation helper containing
the required string release is present elsewhere in the module, so this is an
omitted invocation rather than an unsupported component operation.

**Intended result**

At procedure exit, lowering must deallocate `x%payload` even though `x` itself
is neither allocatable nor finalizable. The generated function should invoke
the derived-storage cleanup helper, or emit equivalent recursive component
cleanup, before `ret`.

**Consequence**

Each invocation leaks the component allocation. The omission generalizes to
ordinary derived locals with nested allocatable storage, so long-running code
can retain recursively owned allocations on every scope exit.

**Confidence:** High. The allocation operation, generated cleanup capability,
and complete absence of a cleanup call in the owning function are all visible
in the same IR module.

### 6. Deferred-length character function result temporary is not released by the caller

**Source locations**

- `src/ir/lower/core.rs:25510-25536` allocates a hidden 32-byte result
  descriptor, calls a deferred-length character function, and returns only the
  loaded data pointer and length to expression lowering.
- `src/ir/lower/core.rs:24662-24708` classifies ownership using expression
  syntax and recognizes concatenation plus a short intrinsic list, but not an
  arbitrary user-defined character function result.
- `src/ir/lower/core.rs:24711-24721` emits temporary deallocation only when that
  classifier reports ownership.

**Example** (`/tmp/armfortas-audit-cases/char_function_temp.f90`)

```fortran
module char_function_temp_m
  implicit none
contains
  function make_text() result(text)
    character(:), allocatable :: text
    text = 'temporary result'
  end function
end module

program p
  use char_function_temp_m
  implicit none
  character(32) :: sink
  sink = make_text()
  print *, trim(sink)
end program
```

**Commands**

```sh
cd /tmp/armfortas-audit
./target/debug/armfortas -O0 --emit-ir -o /tmp/a03-char-result.ir \
  /tmp/armfortas-audit-cases/char_function_temp.f90
./target/debug/armfortas -O0 -o /tmp/a03-char-result \
  /tmp/armfortas-audit-cases/char_function_temp.f90
/tmp/a03-char-result
sed -n '/func @main/,/^  }/p' /tmp/a03-char-result.ir
```

**Actual result**

The visible value is correct and prints `temporary result`. In the caller IR,
lowering allocates and zeroes a hidden result descriptor, calls `make_text`,
loads its pointer and length, and copies into `sink`. The caller then proceeds
to output and returns. There is no `afs_dealloc_string` or
`@__afs_deallocate` call for the hidden result buffer.

The callee's own cleanup does not cover this allocation: it copies/moves its
result into the caller-provided hidden descriptor, and the resulting buffer is
owned by that descriptor after the call.

**Intended result**

Once assignment has copied the function value into `sink`, the caller must
release the owned deferred-length result temporary and clear or retire its
descriptor.

**Consequence**

Every evaluated user-defined deferred-length character result leaks its result
buffer after use. Repeated calls and nested expressions accumulate memory even
though the computed text is correct.

**Confidence:** High. The caller-side ownership allocation and the missing
release are explicit in O0 IR, and the syntax-based ownership classifier omits
this result category.

### 7. Imported module LOGICAL(kind) array loses logical type metadata during lowering

**Source locations**

- `src/ir/lower/core.rs:3080-3109` defines `ModuleGlobalInfo` without a
  `logical_kind` field.
- `src/ir/lower/core.rs:7881-7931` reconstructs imported module globals as
  `LocalInfo` with `logical_kind: None`.
- `src/ir/lower/core.rs:33511-33575` chooses whole-array scalar output using
  `info.logical_kind.is_some()`, so the imported logical elements take the
  integer output path.

**Example** (`/tmp/armfortas-audit-cases/module_logical_array.f90`)

```fortran
module module_logical_array_m
  implicit none
  logical(1) :: flags(2) = [.true._1, .false._1]
end module

program p
  use module_logical_array_m
  implicit none
  print *, flags
end program
```

**Commands**

```sh
cd /tmp/armfortas-audit
./target/debug/armfortas -O0 --emit-ir -o /tmp/a03-module-logical.ir \
  /tmp/armfortas-audit-cases/module_logical_array.f90
./target/debug/armfortas -O0 -o /tmp/a03-module-logical \
  /tmp/armfortas-audit-cases/module_logical_array.f90
/tmp/a03-module-logical
gfortran -O0 -o /tmp/a03-module-logical-gfortran \
  /tmp/armfortas-audit-cases/module_logical_array.f90
/tmp/a03-module-logical-gfortran
```

**Actual result**

Armfortas prints the elements as integer values, normalized as:

```text
1 0
```

The local reference compiler prints logical values:

```text
T F
```

The Armfortas IR correctly stores the global as `[i8 x 2] = [1, 0]`, but the
output loop calls `@afs_write_int8` for each element rather than a logical
writer.

**Intended result**

The module-global metadata must preserve both the physical kind and the source
logical category. Whole-array output must format these elements as logical
values, producing `T F` rather than integer digits.

**Consequence**

Type identity is lost at the module-global to local-symbol boundary. This
already changes formatted/list-directed output and can affect any later
lowering decision that distinguishes logical storage from same-width integer
storage through `LocalInfo.logical_kind`.

**Confidence:** High. The metadata assignment, selected runtime writer, and
observable output all identify the same category-erasure defect.

### 8. Nondefault-kind LOGICAL condition produces a non-Boolean conditional branch

**Source locations**

- `src/ir/lower/core.rs:27151-27192` lowers the ordinary condition path and
  supplies the raw expression value to `cond_br` without normalizing a
  nondefault logical representation to IR `bool`.
- `src/ir/verify.rs:157-191` validates terminator structure but does not require
  a `CondBranch` condition to have `Type::Bool`.
- `src/ir/verify.rs:289-359` checks branch arguments and targets, not the
  conditional operand's type.

**Example** (`/tmp/armfortas-audit-cases/logical_kind_condition.f90`)

```fortran
program p
  implicit none
  logical(1) :: flag
  flag = .true.
  if (flag) print *, 'taken'
end program
```

**Commands**

```sh
cd /tmp/armfortas-audit
./target/debug/armfortas -O0 --emit-ir -o /tmp/a03-logical-cond.ir \
  /tmp/armfortas-audit-cases/logical_kind_condition.f90
./target/debug/armfortas -O0 -o /tmp/a03-logical-cond \
  /tmp/armfortas-audit-cases/logical_kind_condition.f90
/tmp/a03-logical-cond
```

**Actual result**

Compilation and verification succeed, and the current backend happens to print
`taken`. The typed IR loads an `i8` and uses it directly as the condition:

```text
%4 = load %0 : i8
cond_br %4, if_then_2(), if_end_1()
```

**Intended result**

Lowering should normalize Fortran logical storage to the IR condition type,
for example:

```text
%is_true = icmp ne %4, 0 : bool
cond_br %is_true, if_then_2(), if_end_1()
```

Independently, the verifier should reject a `CondBranch` whose condition is not
`bool`.

**Consequence**

The module violates its own typed-IR boundary while still being declared
valid. Current execution relies on a backend accepting integer truth values;
optimizer passes or another backend can legitimately assume a Boolean operand
and produce different behavior or fail later.

**Confidence:** High. The operand type and verifier acceptance are directly
visible. The correct runtime output does not remove the invalid IR contract.

## Printed IR stability

No printed-IR instability was reproduced. The following local sweep emitted
the same ordinary Fortran program 12 times at each optimization level and
compared SHA-256 values:

```sh
cd /tmp/armfortas-audit
for opt in 0 2; do
  for i in $(seq 1 12); do
    ./target/debug/armfortas -O"$opt" --emit-ir \
      -o "/tmp/a03-det-${opt}-${i}.ir" \
      test_programs/ar13_fgof_watch_deterministic.f90
    sha256sum "/tmp/a03-det-${opt}-${i}.ir"
  done
done
```

Each optimization level produced exactly one unique hash across its 12 runs.
This is evidence for process-to-process stability on that input, not a proof
that every IR printer path is deterministic.

## Code-organization observations

1. `src/ir/lower/core.rs` is 59,483 lines and `src/ir/lower/stmt.rs` is 9,356
   lines. Expression ownership, descriptor ABI construction, implicit cleanup,
   I/O category selection, and CFG construction are interleaved across these
   files. The deferred-character defect is a concrete example: expression
   lowering creates an owned result, while a later AST-shape classifier tries
   to rediscover ownership instead of receiving an ownership/cleanup result
   from the lowering operation.

2. Scope cleanup is emitted manually at several syntactic endpoints rather
   than represented as scope-exit actions attached to every outgoing CFG edge.
   Relevant duplicated paths include `src/ir/lower/unit.rs:203-216`,
   `src/ir/lower/unit.rs:718-730`, `src/ir/lower/unit.rs:1536-1590`,
   `src/ir/lower/stmt.rs:5979-6201`, and
   `src/ir/lower/stmt.rs:7467-7505`. This structure permits ordering drift and
   makes direct transfers such as `GOTO` bypass cleanup unless special-cased.

3. Semantic metadata has parallel, lossy representations. `LocalInfo` carries
   logical-category data that `ModuleGlobalInfo` does not, while `TypeLayout`
   keeps final procedure names without rank or ABI. These are not merely
   cosmetic duplication: both losses lead directly to confirmed discrepancies
   above.

4. `verify_module` delegates to isolated function verification without a module
   symbol/signature environment. Consequently it cannot validate direct-call
   arity and argument types against locally defined callees. Terminator checks
   similarly omit the condition-type invariant. The verifier therefore marks
   malformed but backend-sensitive IR as valid.

## Missing-test coverage gaps

1. **Derived ownership cleanup.** The implicit-deallocation unit coverage near
   `src/ir/lower/core.rs:59245-59263` checks a directly allocatable array, but
   not a nonallocatable derived local with allocatable or deferred-character
   components, nested components, or repeated-call leak behavior.

2. **FINAL ordering, object address, and rank ABI.**
   `test_programs/derived_type_final.f90:1-27` and existing block-finalization
   cases exercise scalar, nonallocatable objects with simple scalar fields.
   They do not cover an allocatable finalized object, a finalizer observing an
   allocated component, rank-specific final procedures, descriptor extents, or
   selection among final procedures of different ranks.

3. **Construct exits and cleanup edges.** The associate coverage near
   `src/ir/lower/core.rs:59202-59216` checks fallthrough, and
   `tests/cli_driver.rs:14570-14605` checks named `EXIT` from `BLOCK`. There is
   no behavioral test for named `EXIT` from `ASSOCIATE`, direct or computed
   `GOTO` leaving a `BLOCK`, or cleanup/finalization on every legal edge out of
   a scoped construct.

4. **Owned character expression results.** The deferred-character result case
   near `tests/cli_driver.rs:38818-38855` checks the computed value only. It
   does not inspect the caller IR for release of the hidden result, count
   allocations across repeated calls, or cover user function results nested in
   concatenation and intrinsic expressions.

5. **Negative verifier invariants.** The positive conditional-branch case near
   `src/ir/verify.rs:889-905` uses a Boolean operand. There are no rejection
   tests for integer-valued `cond_br`, mismatched return values, or direct calls
   whose argument types disagree with a defined callee signature.

6. **Logical category across module globals.** Existing kind/storage coverage
   does not print an imported whole array of `LOGICAL(1)` or assert that module
   installation preserves `logical_kind`. A paired local-array/module-array
   output test would expose the metadata boundary immediately.

7. **Raw printed-IR determinism.** `tests/determinism_sweep.rs:1-213` compares
   optimized assembly output. It does not compare `--emit-ir` output across
   fresh compiler processes, at both O0 and optimized levels, or on modules
   containing generated derived-type helpers and multiple CFG targets.
