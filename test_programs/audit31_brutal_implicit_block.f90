! XFAIL: IMPLICIT inside BLOCK not honored — Task #497
! CHECK: 42
! audit31: IMPLICIT statement inside BLOCK not honored (F2008 §7.5.6).
! Inside a BLOCK we may locally redefine implicit typing; if the outer
! scope was IMPLICIT NONE, inside the block we can re-enable it.
! Expected: compile and print 15.
! Actual:   "error: variable 'n' used but not declared (IMPLICIT NONE is active)".
program audit31_implicit_block
  implicit none
  integer :: a
  a = 5
  block
    implicit integer (i-n)
    n = a + 10
    print *, n
  end block
end program
