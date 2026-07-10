# Audit 07: Fortran resource-management semantics

## Scope and method

This review is limited to ordinary Fortran behavior for ALLOCATE, DEALLOCATE,
MOVE_ALLOC, allocatable assignment, derived-component cleanup, character
allocation, finalization, STAT, and ERRMSG. It covers the Rust frontend,
lowering, runtime, existing tests, focused Fortran programs, normal formatted
text output, and O0 textual IR. It does not assess coarrays, raw storage
representations, or non-language concerns.

The reviewed implementation was at commit
`23857aa48f3bc0160303842488e8578acb487fb1`. Runtime comparisons used:

~~~text
armfortas 0.1.0 (x86_64-linux-gnu)
GNU Fortran 16.1.1 20260625
~~~

Typical reproduction commands were:

~~~sh
target/debug/armfortas -O0 repro.f90 -o /tmp/repro.afs
/tmp/repro.afs
target/debug/armfortas -O0 --emit-ir repro.f90 -o /tmp/repro.ir
gfortran repro.f90 -o /tmp/repro.gf
/tmp/repro.gf
~~~

GNU Fortran was used as a secondary behavioral cross-check. The intended
behavior below follows the Fortran allocation, intrinsic-assignment, and
finalization rules, not implementation-specific diagnostic wording or status
numbers.

Severity is about ordinary program correctness and resource lifecycle:

- **High**: silently changes allocation/finalization state, suppresses required
  termination, loses a value, or skips required cleanup.
- **Medium**: rejects a valid recovery form, accepts an invalid statement, or
  changes observable metadata/status without otherwise losing the value in the
  focused case.

## Summary

| ID | Severity | Discrepancy |
| --- | --- | --- |
| RM-01 | High | Omitting STAT suppresses required allocation error termination |
| RM-02 | High | DEALLOCATE of an unallocated descriptor reports success |
| RM-03 | High | Later objects overwrite an earlier multi-object ALLOCATE error |
| RM-04 | High | Deferred-character and pointer DEALLOCATE paths do not define STAT/ERRMSG |
| RM-05 | Medium | Scalar array elements are rejected as STAT/ERRMSG variables |
| RM-06 | Medium | Duplicate and DEALLOCATE-inapplicable options are accepted |
| RM-07 | High | Reallocating an allocated deferred-length character falsely succeeds |
| RM-08 | High | Fixed-length character ALLOCATE with SOURCE does not copy the source |
| RM-09 | Medium | Conforming character-array assignment changes retained bounds |
| RM-10 | Medium | MOVE_ALLOC ignores its successful STAT result |
| RM-11 | High | MOVE_ALLOC replaces an allocated TO without finalization |
| RM-12 | High | Allocatable derived-array assignment omits LHS finalization |
| RM-13 | High | Fixed arrays of allocatable-bearing components are shallow-copied |
| RM-14 | High | Implicit cleanup finalizes allocatable objects after destroying them |
| RM-15 | High | Owned allocatable components are not recursively finalized and cleaned |
| RM-16 | High | FINAL metadata and dispatch are rank-blind |
| RM-17 | High | Parent and dynamic-type finalization are omitted |
| RM-18 | High | Allocation-size overflow aborts or falsely succeeds despite STAT |

## Verified discrepancies

### RM-01 — Omitting STAT suppresses required error termination

**Severity:** High
**Confidence:** High

**Source locations:** src/ir/lower/core.rs:55371-55381 always creates an
i32 scratch address when STAT is absent. The ALLOCATE and DEALLOCATE lowering
passes that non-null address at src/ir/lower/stmt.rs:6209-6222,
src/ir/lower/stmt.rs:6884-6905, and src/ir/lower/stmt.rs:7180-7293. The runtime
only terminates on these errors when its status pointer is null at
runtime/src/array.rs:970-980 and runtime/src/array.rs:1508-1518.

**Focused reproduction:**

~~~fortran
program p
  implicit none
  integer, allocatable :: a(:)
  allocate(a(1))
  print *, 'before'
  allocate(a(2))
  print *, 'after', size(a)
end program
~~~

**Actual behavior:** armfortas exits successfully and prints:

~~~text
 before
 after           1
~~~

The textual IR passes a scratch slot even though the source has no STAT:

~~~text
%29 = alloca i32
store 0, %29
call @afs_allocate_array(..., %29)
~~~

The same pattern lets DEALLOCATE of an unallocated array continue when STAT is
absent.

**Intended behavior:** An allocation or deallocation error without STAT
initiates error termination; the second line must not execute.

**Consequence:** Execution continues with the old allocation state after a
failed memory-management statement.

### RM-02 — DEALLOCATE of an unallocated descriptor reports success

**Severity:** High
**Confidence:** High

**Source location:** runtime/src/array.rs:1492-1518 explicitly treats an
unallocated descriptor as a successful no-op when the status pointer is
present, writing zero at lines 1510-1513.

**Focused reproduction:**

~~~fortran
program p
  implicit none
  integer, allocatable :: a(:)
  integer :: stat
  character(64) :: msg
  stat = -99
  msg = 'seed'
  deallocate(a, stat=stat, errmsg=msg)
  print '(i0,1x,a)', stat, trim(msg)
end program
~~~

**Actual behavior:**

~~~text
0 seed
~~~

**Intended behavior:** Deallocating an unallocated allocatable is an error
condition. STAT must become positive and ERRMSG must receive explanatory text.

**Consequence:** Callers cannot distinguish invalid cleanup from success and
can take a success branch after no deallocation occurred.

### RM-03 — A later object overwrites an earlier multi-object ALLOCATE error

**Severity:** High
**Confidence:** High

**Source locations:** src/ir/lower/stmt.rs:6209-6222 creates one shared status
slot and src/ir/lower/stmt.rs:6257-7176 reuses it for every allocation object.
Each successful runtime call writes zero, including
runtime/src/array.rs:1046-1049.

**Focused reproduction:**

~~~fortran
program p
  implicit none
  integer, allocatable :: a(:), b(:)
  integer :: stat
  character(40) :: msg
  allocate(a(1))
  msg = 'unchanged'
  allocate(a(2), b(3), stat=stat, errmsg=msg)
  print '(i0,1x,l1,1x,a)', stat, allocated(b), trim(msg)
  if (allocated(b)) print '(a,i0)', 'b-size=', size(b)
end program
~~~

**Actual behavior:**

~~~text
0 T ALLOCATE failed
b-size=3
~~~

The IR lets the first call set the shared slot and ERRMSG, then lets the second
successful call reset that slot before statement-level writeback.

**Intended behavior:** Once the statement has encountered an allocation error,
its final STAT must be nonzero and ERRMSG must describe that error. Whether a
processor attempts a later object does not make the earlier error disappear.

**Consequence:** STAT says success while ERRMSG says failure, so normal
status-based recovery takes the wrong branch.

### RM-04 — Deferred-character and pointer DEALLOCATE paths do not define STAT or ERRMSG

**Severity:** High
**Confidence:** High

**Source locations:** Deferred character components and locals directly call
the status-less string routine at src/ir/lower/stmt.rs:7193-7199 and
src/ir/lower/stmt.rs:7261-7267. Pointer components and locals directly free and
null their slots at src/ir/lower/stmt.rs:7237-7254 and
src/ir/lower/stmt.rs:7301-7313. runtime/src/string.rs:123-140 has no status or
message result.

**Focused reproduction:**

~~~fortran
program p
  implicit none
  character(:), allocatable :: s
  integer :: stat
  character(40) :: msg
  s = 'abc'
  stat = -7; msg = 'seed'
  deallocate(s, stat=stat, errmsg=msg)
  print '(i0,1x,l1,1x,a)', stat, allocated(s), trim(msg)
  stat = -8; msg = 'seed2'
  deallocate(s, stat=stat, errmsg=msg)
  print '(i0,1x,l1,1x,a)', stat, allocated(s), trim(msg)
end program
~~~

**Actual behavior:**

~~~text
-7 F seed
-8 F seed2
~~~

The corresponding pointer case leaves -7 after a successful deallocation and
-8 after attempting to deallocate the now-disassociated pointer.

**Intended behavior:** The successful operation sets STAT to zero. The second
operation sets a positive status and explanatory ERRMSG.

**Consequence:** Both success and error leave stale caller values, defeating
the language's status protocol.

### RM-05 — Scalar array elements are rejected as STAT and ERRMSG variables

**Severity:** Medium
**Confidence:** High

**Source locations:** src/ir/lower/core.rs:55390-55470 accepts only a bare name
or component access for STAT. src/ir/lower/core.rs:55493-55529 imposes the same
expression-shape restriction on ERRMSG.

**Focused reproduction:**

~~~fortran
program p
  implicit none
  integer, allocatable :: a(:)
  integer :: stats(2)
  stats = -9
  allocate(a(2), stat=stats(2))
  print *, stats
end program
~~~

**Actual behavior:** Compilation stops with:

~~~text
ALLOCATE/DEALLOCATE STAT= must name a scalar integer variable
~~~

An analogous ERRMSG=msgs(2), where msgs is a character array, is rejected as
not being a scalar character variable.

**Intended behavior:** An array element is a scalar variable and is a valid
STAT or ERRMSG designator. The focused program compiles and defines stats(2)
to zero.

**Consequence:** Valid standard-conforming status and message storage used by
table-driven recovery code cannot be expressed.

### RM-06 — Duplicate and DEALLOCATE-inapplicable options are accepted

**Severity:** Medium
**Confidence:** High

**Source locations:** src/parser/stmt.rs:1720-1786 accepts STAT, ERRMSG, SOURCE,
and MOLD through the same path for both statements. Validation at
src/sema/validate/core.rs:2759-2793 does not reject duplicates and does not
validate DEALLOCATE options. src/ir/lower/core.rs:55319-55330 silently chooses
the first matching keyword.

**Focused reproduction:**

~~~fortran
program p
  implicit none
  integer, allocatable :: a(:)
  integer :: s1 = -1, s2 = -2
  allocate(a(2), stat=s1, stat=s2)
  print *, s1, s2
end program
~~~

**Actual behavior:** The invalid statement compiles and prints:

~~~text
           0          -2
~~~

Also verified: DEALLOCATE(a, SOURCE=42) compiles, ignores SOURCE, and
deallocates a.

**Intended behavior:** Duplicate option specifiers and SOURCE/MOLD on
DEALLOCATE are constraint violations and must be rejected.

**Consequence:** Misspelled or duplicated recovery clauses are silently
ignored, and source text does not describe the executed semantics.

### RM-07 — Reallocating an allocated deferred-length character falsely succeeds

**Severity:** High
**Confidence:** High

**Source locations:** src/ir/lower/stmt.rs:6748-6766 directly calls
init_allocated_string_descriptor. That helper at
src/ir/lower/core.rs:55073-55096 allocates and overwrites descriptor fields
without checking allocation state or receiving a status address. The
statement-level path pre-zeros STAT at src/ir/lower/stmt.rs:6219-6222.

**Focused reproduction:**

~~~fortran
program p
  implicit none
  character(:), allocatable :: s
  integer :: stat
  character(40) :: msg
  allocate(character(3) :: s)
  s = 'abc'
  stat = -9; msg = 'unchanged'
  allocate(character(5) :: s, stat=stat, errmsg=msg)
  print '(i0,1x,i0,1x,l1,1x,a)', stat, len(s), s == 'abc', trim(msg)
end program
~~~

**Actual behavior:**

~~~text
0 5 F unchanged
~~~

The IR performs a second raw allocation and descriptor overwrite with no
allocated-state branch.

**Intended behavior:** The second ALLOCATE is an error. STAT is positive,
ERRMSG explains the already-allocated object, and the original length-three
value remains allocated and equal to abc.

**Consequence:** The error is reported as success, the value changes, and the
old allocation is no longer represented by the variable.

### RM-08 — Fixed-length character ALLOCATE with SOURCE does not copy the source

**Severity:** High
**Confidence:** High

**Source locations:** The rank-zero SOURCE path at
src/ir/lower/stmt.rs:6925-6969 copies non-character sources and class-star
character sources but has no ordinary character branch. The duplicated
component path has the same omission at src/ir/lower/stmt.rs:6492-6535.

**Focused reproduction:**

~~~fortran
program p
  implicit none
  character(5), allocatable :: a
  character(3), allocatable :: b
  allocate(a, source='abcde')
  allocate(b, source='xyz')
  print '(i0,1x,l1)', len(a), a == 'abcde'
  print '(i0,1x,l1)', len(b), b == 'xyz'
end program
~~~

**Actual behavior:**

~~~text
5 F
3 F
~~~

Textual IR contains the descriptor allocations but no character assignment or
copy for either SOURCE expression.

**Intended behavior:**

~~~text
5 T
3 T
~~~

SOURCE allocation defines each allocation object from its corresponding
source value.

**Consequence:** ALLOCATE reports success and supplies the right length while
discarding the requested initial value.

### RM-09 — Conforming character-array assignment changes retained bounds

**Severity:** Medium
**Confidence:** High

**Source locations:** lower_allocatable_char_array_assign_from_desc at
src/ir/lower/core.rs:47610-47637 unconditionally deallocates and reallocates the
left side. The fixed-character allocation-like helper creates one-based bounds
at runtime/src/array.rs:1080-1112.

**Focused reproduction:**

~~~fortran
program p
  implicit none
  character(3), allocatable :: lhs(:), rhs(:)
  allocate(lhs(0:1), rhs(5:6))
  rhs(5) = 'abc'; rhs(6) = 'def'
  lhs = rhs
  print '(i0,1x,i0)', lbound(lhs,1), ubound(lhs,1)
end program
~~~

**Actual behavior:**

~~~text
1 2
~~~

The IR explicitly calls afs_deallocate_array(lhs) followed by
afs_allocate_like_with_elem_size(lhs,...).

**Intended behavior:**

~~~text
0 1
~~~

Because shape, type, kind, and character length already conform, allocatable
assignment retains the existing allocation and its bounds.

**Consequence:** Subsequent indexing and associations observe a different
allocation identity and lower bound even though reallocation was not required.

### RM-10 — MOVE_ALLOC ignores its successful STAT result

**Severity:** Medium
**Confidence:** High

**Source locations:** src/ir/lower/intrinsic_sub.rs:281-328 reads only FROM and
TO and emits a two-argument runtime call. Keyword ordering lists only those two
arguments at src/ir/lower/core.rs:18663-18666 even though the validator permits
up to four MOVE_ALLOC arguments at src/sema/validate/core.rs:4205.

**Focused reproduction:**

~~~fortran
program p
  implicit none
  integer, allocatable :: from(:), to(:)
  integer :: stat
  allocate(from(1))
  from = 42
  stat = -9
  call move_alloc(from, to, stat=stat)
  print '(i0,1x,l1,1x,l1,1x,i0)', stat, allocated(from), &
       allocated(to), to(1)
end program
~~~

**Actual behavior:**

~~~text
-9 F T 42
~~~

The IR stores -9, calls afs_move_alloc with two descriptors, then reloads the
unchanged variable.

**Intended behavior:**

~~~text
0 F T 42
~~~

A present STAT argument is defined to zero on successful completion.

**Consequence:** A successful ownership transfer leaves a stale failure value.

### RM-11 — MOVE_ALLOC replaces an allocated TO without finalization

**Severity:** High
**Confidence:** High

**Source locations:** src/ir/lower/intrinsic_sub.rs:319-328 directly invokes
the runtime. runtime/src/array.rs:2072-2093 directly releases TO's outer
storage and clones FROM's descriptor, with no final or component-cleanup call.

**Focused reproduction:**

~~~fortran
module m
  implicit none
  integer :: finalized = 0
  type :: t
    integer :: id = 0
  contains
    final :: finish
  end type
contains
  subroutine finish(x)
    type(t), intent(inout) :: x
    finalized = finalized + x%id
  end subroutine
end module
program p
  use m
  implicit none
  type(t), allocatable :: from, to
  allocate(from, to)
  from%id = 7; to%id = 3
  call move_alloc(from, to)
  print *, finalized, allocated(from), allocated(to), to%id
end program
~~~

**Actual behavior:**

~~~text
           0 F T           7
~~~

Textual IR contains call @afs_move_alloc with no call to finish before the
print.

**Intended behavior:** TO's old value is finalized as part of making TO
unallocated, so finalized is 3 before the transferred value is observed.

**Consequence:** Final side effects and normal cleanup of allocatable
components owned by the replaced TO are skipped.

### RM-12 — Allocatable derived-array assignment omits LHS finalization

**Severity:** High
**Confidence:** High

**Source locations:** src/ir/lower/core.rs:48019-48023 considers only deep-copy
needs, not finalization. The non-deep-copy path at
src/ir/lower/core.rs:48367-48453 calls afs_assign_allocatable directly. The
deep-copy path also directly deallocates the destination at
src/ir/lower/core.rs:48493-48510. runtime/src/array.rs:1576-1662 has no
Fortran finalization callback.

**Focused reproduction:**

~~~fortran
module m
  implicit none
  integer :: finalized = 0
  type :: t
    integer :: id = 0
  contains
    final :: finish_rank1
  end type
contains
  subroutine finish_rank1(x)
    type(t), intent(inout) :: x(:)
    finalized = finalized + sum(x%id)
  end subroutine
end module
program p
  use m
  implicit none
  type(t), allocatable :: lhs(:), rhs(:)
  allocate(lhs(2), rhs(2))
  lhs%id = [3,4]; rhs%id = [7,8]
  lhs = rhs
  print *, finalized, lhs%id
end program
~~~

**Actual behavior:**

~~~text
           0           7           8
~~~

The IR has call @afs_assign_allocatable(lhs,rhs) and no finish_rank1 call.

**Intended behavior:**

~~~text
           7           7           8
~~~

The old finalizable LHS is finalized before it is defined by intrinsic
assignment.

**Consequence:** Assignment-time final actions and resource lifecycle work do
not run for allocatable derived arrays.

### RM-13 — Fixed arrays of allocatable-bearing components are shallow-copied

**Severity:** High
**Confidence:** High

**Source locations:** derived_layout_needs_deep_copy at
src/ir/lower/core.rs:52633-52652 only recurses into a nonallocatable derived
component when field.dims is empty. emit_derived_value_copy at
src/ir/lower/core.rs:54403-54410 then selects a whole-value memcpy; the inline
fallback at src/ir/lower/core.rs:54559-54572 also recurses only for scalar
components.

**Focused reproduction:**

~~~fortran
module m
  implicit none
  type :: leaf
    character(:), allocatable :: text
  end type
  type :: outer
    type(leaf) :: item(2)
  end type
  type(outer) :: a, b
end module
program p
  use m
  implicit none
  a%item(1)%text = 'one'
  a%item(2)%text = 'two'
  b = a
  a%item(1)%text = 'ONE'
  print '(a)', b%item(1)%text
end program
~~~

**Actual behavior:**

~~~text
ONE
~~~

The O0 IR implements b=a as memcpy of the complete 64-byte outer value.

**Intended behavior:**

~~~text
one
~~~

Intrinsic assignment deep-copies allocatable components recursively, including
those inside a fixed-size derived array component.

**Consequence:** The two assigned values are not independent; later
definitions and cleanup operate on the same component allocation.

### RM-14 — Implicit cleanup finalizes allocatable objects after destroying them

**Severity:** High
**Confidence:** High

**Source locations:** insert_implicit_dealloc releases components and outer
descriptor storage at src/ir/lower/core.rs:26536-26555, then calls
finalize_derived_storage on info.addr at lines 26557-26570. It does not use the
allocated guard and payload walk available at src/ir/lower/core.rs:26413-26453.

**Focused reproduction:**

~~~fortran
module m
  implicit none
  type :: t
    integer :: value = -1
  contains
    final :: finish
  end type
contains
  subroutine finish(x)
    type(t), intent(inout) :: x
    print '(a,i0)', 'final=', x%value
  end subroutine
  subroutine exercise
    type(t), allocatable :: x
    allocate(x)
    x%value = 37
  end subroutine
end module
program p
  use m
  call exercise
end program
~~~

**Actual behavior:**

~~~text
final=0
~~~

The IR order is:

~~~text
call @afs_deallocate_array(%0, %49)
call @afs_modproc_m_finish(%0)
~~~

If exercise explicitly deallocates x before returning, armfortas invokes the
finalizer once for value 91 and again at exit for value 0; the intended count
is one.

**Intended behavior:** Finalize the allocated payload while its value and
components are intact, then release it. An already-unallocated entity is not
finalized again.

**Consequence:** Finalizers observe destroyed state, and explicit deallocation
can be followed by a second spurious final call.

### RM-15 — Owned allocatable components are not recursively finalized and cleaned

**Severity:** High
**Confidence:** High

**Source locations:** The implicit-cleanup selection at
src/ir/lower/core.rs:26472-26492 recognizes only a top-level allocatable/string
or a type's own direct FINAL list. A nonallocatable owner with an owned
allocatable component is skipped. The component walkers at
src/ir/lower/core.rs:52985-53150 recurse only through deallocation operations;
they never invoke nested finalizers.

**Focused reproduction:**

~~~fortran
module m
  implicit none
  integer :: hits = 0
  type :: inner_t
    integer :: value = -1
  contains
    final :: finish_inner
  end type
  type :: outer_t
    type(inner_t), allocatable :: child
  end type
contains
  subroutine finish_inner(x)
    type(inner_t), intent(inout) :: x
    hits = hits + 1
  end subroutine
  subroutine exercise
    type(outer_t) :: x
    allocate(x%child)
    x%child%value = 41
  end subroutine
end module
program p
  use m
  call exercise
  print '(i0)', hits
end program
~~~

**Actual behavior:**

~~~text
0
~~~

exercise's IR contains no cleanup call. Explicitly deallocating an allocated
outer object was also verified to call the outer finalizer while freeing the
child without its finalizer.

**Intended behavior:**

~~~text
1
~~~

At scope exit, the owned child is finalized and deallocated. With an outer
FINAL, the outer procedure runs first and recursive component finalization
follows.

**Consequence:** Nested final actions are skipped, and some nonallocatable
owners retain their component allocations until process termination.

### RM-16 — FINAL metadata and dispatch are rank-blind

**Severity:** High
**Confidence:** High

**Source locations:** src/parser/decl.rs:995-999 consumes only one name from a
FINAL name list. TypeLayout stores only Vec<String> at
src/sema/type_layout.rs:80-95, and .amod emits only names at
src/sema/amod.rs:1258-1260. finalize_derived_storage calls every stored name
with one address at src/ir/lower/core.rs:26342-26362. Array finalization at
src/ir/lower/core.rs:26365-26409 loops over elements instead of selecting the
rank-matching final subroutine.

**Focused reproduction:**

~~~fortran
module m
  implicit none
  integer :: calls = 0
  type :: t
    integer :: id = 0
  contains
    final :: finish_rank1
  end type
contains
  subroutine finish_rank1(x)
    type(t), intent(inout) :: x(:)
    calls = calls + 1
  end subroutine
end module
program p
  use m
  implicit none
  type(t), allocatable :: a(:)
  allocate(a(2))
  deallocate(a)
  print '(i0)', calls
end program
~~~

**Actual behavior:**

~~~text
2
~~~

IR enters derived_array_final_body for each element and passes an element
address to finish_rank1 each time.

**Intended behavior:**

~~~text
1
~~~

The rank-one final subroutine is selected once for the rank-one entity. An
elemental scalar FINAL, by contrast, is applied element-wise. A valid comma
list FINAL :: scalar, vector must retain both procedure names.

**Consequence:** Valid final procedures run the wrong number of times and can
receive an address with the wrong procedure ABI.

### RM-17 — Parent and dynamic-type finalization are omitted

**Severity:** High
**Confidence:** High

**Source locations:** finalize_derived_storage at
src/ir/lower/core.rs:26342-26362 calls only the selected layout's direct
final_procs and never follows TypeLayout.parent. Explicit deallocation chooses
info.derived_type, the declared type, at src/ir/lower/stmt.rs:7270-7288.
Finalization is not represented in the dynamic-dispatch table built at
src/ir/lower/core.rs:405-470.

**Focused reproduction:**

~~~fortran
module m
  implicit none
  integer :: child_hits = 0, parent_hits = 0
  type :: parent_t
  contains
    final :: finish_parent
  end type
  type, extends(parent_t) :: child_t
  contains
    final :: finish_child
  end type
contains
  subroutine finish_parent(x)
    type(parent_t), intent(inout) :: x
    parent_hits = parent_hits + 1
  end subroutine
  subroutine finish_child(x)
    type(child_t), intent(inout) :: x
    child_hits = child_hits + 1
  end subroutine
  subroutine exercise
    type(child_t) :: x
  end subroutine
end module
program p
  use m
  call exercise
  print '(i0,1x,i0)', child_hits, parent_hits
end program
~~~

**Actual behavior:**

~~~text
1 0
~~~

A second verified case used class(parent_t), allocatable, allocated it as
child_t, and explicitly deallocated it; armfortas invoked only the statically
declared parent finalizer.

**Intended behavior:**

~~~text
1 1
~~~

The most-derived finalizer runs first, followed by finalization of the parent
component. A polymorphic allocatable uses its dynamic type.

**Consequence:** Parent cleanup invariants and the most-derived cleanup for
polymorphic allocations are skipped.

### RM-18 — Allocation-size overflow aborts or falsely succeeds despite STAT

**Severity:** High
**Confidence:** Certain

**Source locations:** `ArrayDescriptor::total_elements` and `total_bytes` use
unchecked i64 multiplication at `runtime/src/descriptor.rs:110-121`.
`afs_allocate_array` repeats unchecked multiplication at
`runtime/src/array.rs:1009-1013`; a wrapped nonpositive result is then treated
as a successful zero-sized allocation at lines 1013-1022.

**Focused reproduction:**

~~~fortran
program p
  integer(1), allocatable :: a(:,:)
  integer :: s
  allocate(a(3037000500_8,3037000500_8), stat=s)
  print *, s, allocated(a)
  if (allocated(a)) deallocate(a)
end program
~~~

~~~sh
target/release/armfortas -O2 repro.f90 -o /tmp/overflow-debug-rt
/tmp/overflow-debug-rt

AFS_RUNTIME_PATH=target/release/libarmfortas_rt.a \
  target/release/armfortas -O2 repro.f90 -o /tmp/overflow-release-rt
/tmp/overflow-release-rt
~~~

**Actual behavior:** With the debug runtime selected by the driver, the first
dimension-product overflow panics inside `afs_allocate_array`; because it is an
`extern "C"` entry point, the panic cannot unwind and the process aborts despite
the source STAT variable. With the release runtime explicitly selected, the
product wraps negative, the runtime prints `0 T`, and the descriptor is marked
allocated with a null payload. GNU Fortran 16 reports `5014 F` for the same
source.

**Intended behavior:** Overflow and unrepresentable allocation sizes are
allocation failures. With STAT present, return a nonzero status and leave the
entity unallocated; without STAT, perform controlled error termination.

**Consequence:** A valid recovery path either aborts the process or receives a
false success and an unusable allocated descriptor. Later size, assignment, or
deallocation operations then act on corrupted allocation state.

## Maintainability notes

- AllocateStatTarget cannot represent the absence of STAT, although the
  runtime ABI already distinguishes null from non-null status pointers.
- Statement status is mutated separately by every allocation object. A
  statement-level first-error result would avoid RM-03 and centralize ERRMSG.
- Descriptor arrays, deferred strings, and pointers have separate
  deallocation paths with different status behavior.
- ALLOCATE lowering duplicates local-variable and component-variable logic;
  the fixed-character SOURCE omission exists in both branches.
- IoControl is shared by I/O and memory statements, leaving allocation-option
  legality to scattered consumers.
- Final metadata is only a list of names. Rank, elemental status, declared
  dummy ABI, parent traversal, and dynamic dispatch cannot be recovered
  reliably at the call site.
- Teardown is manually composed from direct FINAL calls, component
  deallocation, and outer deallocation at many sites. The ordering differs
  between explicit DEALLOCATE, implicit exit, assignment, INTENT(OUT), and
  MOVE_ALLOC.
- The deep-copy predicate and copier each special-case scalar derived
  components, which let fixed arrays of the same component type take a raw
  aggregate copy.

## Coverage notes

- tests/memory_runtime.rs:174-218 currently codifies RM-02: its second
  DEALLOCATE of the same array requires STAT=0 and unchanged ERRMSG.
- tests/memory_runtime.rs:52-145 covers integer-array allocation failures with
  fixed/deferred ERRMSG targets. The deferred test uses character only as the
  message variable; it does not allocate a deferred-length character object.
- Runtime MOVE_ALLOC tests at runtime/src/array.rs:2516-2544 and
  runtime/src/string.rs:929-944 cover transfer and descriptor tags, not STAT,
  finalization, or component cleanup of an allocated TO.
- Existing character-array assignment cases use default one-based bounds, so
  unconditional reallocation is not observable.
- Existing SOURCE/MOLD tests cover numeric arrays, scalar numeric values, and
  allocatable components, but not fixed-length allocatable character scalars.
- The end-to-end FINAL cases are overwhelmingly one scalar finalizer per type.
  test_programs/ar2_final_points.f90 usefully covers scalar assignment,
  INTENT(OUT), and function-result points, but not rank-specific FINAL,
  inherited FINAL, polymorphic dynamic type, or allocatable-local exit.
- test_programs/l08_vtable_final_after_dispatch.f90 checks that a finalizer ran
  but deliberately does not require it to observe the original payload, so it
  cannot catch RM-14.
- There is deep-copy coverage for direct allocatable components and arrays of
  derived elements, but no case where a fixed nonallocatable array component
  contains an allocatable component.
- Missing status coverage includes no-STAT fatal paths, successful and invalid
  deferred-character/pointer deallocation, multi-object first-error
  preservation, array-element STAT/ERRMSG designators, and duplicate or
  inapplicable allocation options.

## Positive observations within scope

- Deferred-length scalar character assignment, including self-referential
  reallocation, uses allocate-before-release behavior in
  runtime/src/string.rs:51-115.
- Basic integer-array MOVE_ALLOC transfer and scalar type-tag preservation are
  covered and passed; deferred-length character MOVE_ALLOC also preserved
  allocation state, length, and value in a focused check.
- Numeric allocatable assignment handles shape-driven allocation and
  noncontiguous source traversal in runtime/src/array.rs:1570-1877.
- Existing derived-component helpers do recursively deallocate direct
  allocatable array and deferred-character components when their owner reaches
  a helper; the discrepancies are in finalization, omitted owner entry, and
  fixed-array copy selection.
