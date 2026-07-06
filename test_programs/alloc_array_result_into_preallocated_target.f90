! Regression: assigning an allocatable-array function result into an
! ALREADY-ALLOCATED allocatable target must deallocate the target first.
! The sret "write straight in" optimization passed the target's live
! descriptor as the callee's result slot; the callee's allocate then
! reused/corrupted it. A size-0 target produced a -1 base and SIGSEGV'd
! in the callee's first element write. This is the fgof-* C-interop idiom
! `allocate(c_parent(0)); c_parent = to_c_string(x)` that crashed 7
! libraries. Correct result: to_c("hi") returns a rank-1 array indexed
! 0:2 (3 elements: 'h', 'i', NUL), so size(c) == 3.
!
! CHECK: 3
module acr_mod
  use iso_c_binding, only : c_char, c_null_char
  implicit none
contains
  function to_c(str) result(buf)
    character(len=*), intent(in) :: str
    character(kind=c_char), allocatable :: buf(:)
    integer :: i, n
    n = len(str)
    allocate(buf(0:n))
    do i = 1, n
      buf(i - 1) = str(i:i)
    end do
    buf(n) = c_null_char
  end function
end module

program alloc_array_result_into_preallocated_target
  use acr_mod
  use iso_c_binding, only : c_char
  implicit none
  character(kind=c_char), allocatable :: c(:)
  allocate(c(0))
  c = to_c("hi")
  print '(i0)', size(c)
end program alloc_array_result_into_preallocated_target
