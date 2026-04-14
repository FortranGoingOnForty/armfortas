! XFAIL: allocate(arr(i)%alloc_comp(n)) rejected — Task #491
! CHECK: ok
! audit31: allocate of an allocatable component via an array-element base rejected.
! fortsh/common/performance.f90 uses this for its token pool.
! Expected: compile & run.
! Actual:   "only allocatable or pointer variables can appear in ALLOCATE,
!           but 'token_pools' is neither"
program audit31_alloc_component
  implicit none
  type :: pool_t
    integer, allocatable :: tokens(:)
    integer :: capacity = 0
  end type
  type(pool_t) :: pools(4)
  allocate(pools(1)%tokens(10))
  pools(1)%capacity = 10
  print *, size(pools(1)%tokens), pools(1)%capacity
end program
