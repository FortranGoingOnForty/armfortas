! Whole-array LBOUND/UBOUND return a rank-one integer array, preserve
! descriptor bounds, canonicalize only zero-extent dimensions, and honor KIND.
! CHECK: whole-array bounds ok
! IR_CHECK: call @afs_array_lbound_vector
! IR_CHECK: call @afs_array_ubound_vector
program whole_array_bounds
  implicit none
  integer :: fixed(-2:1, 7:9)
  integer :: empty(-4:-5, 7:9)
  integer, allocatable :: dynamic(:, :)
  integer(1) :: lower1(2)
  integer(2) :: upper2(2)
  integer(8) :: lower8(2), upper8(2)
  integer(16) :: lower16(2)

  if (any(lbound(fixed) /= [-2, 7])) error stop 1
  if (any(ubound(fixed) /= [1, 9])) error stop 2
  if (any(lbound(empty) /= [1, 7])) error stop 3
  if (any(ubound(empty) /= [0, 9])) error stop 4

  allocate(dynamic(-3:0, 5:6))
  if (any(lbound(dynamic) /= [-3, 5])) error stop 5
  if (any(ubound(dynamic) /= [0, 6])) error stop 6

  lower8 = lbound(dynamic, kind=8)
  upper8 = ubound(dynamic, kind=8)
  lower1 = lbound(dynamic, kind=1)
  upper2 = ubound(dynamic, kind=2)
  lower16 = lbound(dynamic, kind=16)
  if (any(lower8 /= [-3_8, 5_8])) error stop 7
  if (any(upper8 /= [0_8, 6_8])) error stop 8
  if (any(lower1 /= [-3_1, 5_1])) error stop 9
  if (any(upper2 /= [0_2, 6_2])) error stop 10
  if (any(lower16 /= [-3_16, 5_16])) error stop 11
  if (kind(lbound(dynamic, dim=1, kind=8)) /= 8) error stop 12
  if (lbound(dynamic, dim=2, kind=8) /= 5_8) error stop 13
  if (ubound(dynamic, dim=1, kind=8) /= 0_8) error stop 14

  print *, 'whole-array bounds ok'
end program whole_array_bounds
