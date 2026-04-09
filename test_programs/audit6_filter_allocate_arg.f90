! Audit #6 probe — USE ONLY filter walker covers ALLOCATE
! argument expressions. Without the audit5 MAJOR-2 fix, the
! `arr(hidden)` size expression silently lowered to const_int 0
! and produced a zero-length allocation.
!
! ERROR_EXPECTED: hidden
module audit6_filter_alloc_mod
  integer :: visible = 1
  integer :: hidden = 999
end module audit6_filter_alloc_mod

program audit6_filter_allocate_arg
  use audit6_filter_alloc_mod, only: visible
  integer, allocatable :: arr(:)
  allocate(arr(hidden))
  print *, size(arr)
end program
