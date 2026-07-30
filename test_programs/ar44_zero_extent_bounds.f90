! LBOUND and UBOUND canonicalize a zero-extent dimension to 1:0 even
! when its descriptor or declaration stores different bounds. Other,
! nonempty dimensions retain their own declared bounds.
!
! FLAGS: --std=f2023
! CHECK: 1 0 7 9
! CHECK: 1 0 7 9
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_array_lbound
! IR_CHECK: call @afs_array_ubound
program ar44_zero_extent_bounds
  implicit none
  integer :: dim
  integer :: fixed_empty(-4:-5, 7:9)
  integer, allocatable :: allocated_empty(:, :)

  if (lbound(fixed_empty, 1) /= 1) error stop 1
  if (ubound(fixed_empty, 1) /= 0) error stop 2
  if (lbound(fixed_empty, 2) /= 7) error stop 3
  if (ubound(fixed_empty, 2) /= 9) error stop 4

  dim = 1
  if (lbound(fixed_empty, dim) /= 1) error stop 5
  if (ubound(fixed_empty, dim) /= 0) error stop 6
  dim = 2
  if (lbound(fixed_empty, dim) /= 7) error stop 7
  if (ubound(fixed_empty, dim) /= 9) error stop 8

  allocate(allocated_empty(-4:-5, 7:9))
  if (lbound(allocated_empty, 1) /= 1) error stop 9
  if (ubound(allocated_empty, 1) /= 0) error stop 10
  if (lbound(allocated_empty, 2) /= 7) error stop 11
  if (ubound(allocated_empty, 2) /= 9) error stop 12

  dim = 1
  if (lbound(allocated_empty, dim) /= 1) error stop 13
  if (ubound(allocated_empty, dim) /= 0) error stop 14

  print *, lbound(fixed_empty, 1), ubound(fixed_empty, 1), &
      lbound(fixed_empty, 2), ubound(fixed_empty, 2)
  print *, lbound(allocated_empty, 1), ubound(allocated_empty, 1), &
      lbound(allocated_empty, 2), ubound(allocated_empty, 2)
end program ar44_zero_extent_bounds
