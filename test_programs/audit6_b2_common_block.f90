! Audit #6 BLOCKING-2 — COMMON blocks read back garbage.
!
! `common /myblock/ a, b` declared in both the program scope
! and a contained subroutine should provide a shared backing
! store: writing 10 / 20 in the program should be visible to
! the subroutine that prints them.
!
! Observed: program prints `1 34276764` (or similar garbage),
! suggesting the COMMON allocation is not actually shared
! between the two scopes — likely each scope is creating its
! own private alloca rather than referencing a single .bss
! global. Fortran 77 baseline feature, in CLAUDE.md "in scope
! and required" list.
!
! XFAIL: audit6 BLOCKING-2 (COMMON blocks not actually shared)
! CHECK: 10 20
program audit6_b2_common_block
  integer :: a, b
  common /myblock/ a, b
  a = 10
  b = 20
  call print_common()
contains
  subroutine print_common()
    integer :: a, b
    common /myblock/ a, b
    print *, a, b
  end subroutine
end program
