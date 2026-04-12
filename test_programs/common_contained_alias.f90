! COMMON block aliased across a contained subroutine.
!
! F77 §5.5 says the mapping from a COMMON block to local names is
! positional — two scopes that declare the same block with different
! local names refer to the same storage, slot by slot.  Earlier the
! module-walker emitted one global per (block, var_name) pair, so
! `common /blk/ a, b` in the host and `common /blk/ x, y` in the
! contained subroutine produced four separate globals instead of
! two, and the subroutine's updates never made it back to the host
! print.
program common_contained_alias
  implicit none
  integer :: a, b
  common /blk/ a, b

  a = 1
  b = 2
  call bump()
  print *, a, b
contains
  subroutine bump()
    implicit none
    integer :: x, y
    common /blk/ x, y
    x = x + 100
    y = y + 100
  end subroutine bump
end program common_contained_alias
! CHECK: 101 102
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
