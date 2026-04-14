! Host association with mixed types: integer + real + logical host
! locals, each needing its own hidden pointer param (typed correctly
! for codegen). Validates build_host_ref_params picks the right
! Ptr(elem_ty) per variable.
! CHECK: 7     3.5000000E0 T
program host_mixed
  implicit none
  integer :: i
  real :: r
  logical :: flag
  i = 0
  r = 0.0
  flag = .false.
  call setup()
  print *, i, r, flag
contains
  subroutine setup()
    i = 7
    r = 3.5
    flag = .true.
  end subroutine
end program
