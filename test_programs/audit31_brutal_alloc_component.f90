! audit31 Finding 10: `allocate(arr(i)%alloc_comp(n))` used to be
! rejected by validate_allocatable_item. It ran extract_base_name
! on the target and checked attrs on the ROOT (`pools`) instead of
! the leaf component (`tokens`). Sema doesn't track per-field
! attribute metadata today, so component-access targets now skip
! the pre-lowering allocatability check; the lowering path still
! requires the leaf to be allocatable. Task #491.
!
! This test verifies ALLOCATE itself is accepted and produces a
! runnable binary. Reading back the allocated data (size() and
! element access through `pools(i)%tokens(j)`) is a distinct
! lowering gap and is deferred.
! CHECK: ok
program audit31_alloc_component
  implicit none
  type :: pool_t
    integer, allocatable :: tokens(:)
    integer :: capacity = 0
  end type
  type(pool_t) :: pools(4)
  allocate(pools(1)%tokens(10))
  print *, 'ok'
end program
