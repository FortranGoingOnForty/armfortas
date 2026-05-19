! CHECK: ok
! IR_CHECK: call @afs_array_pack
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|asm|obj|repro
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program pack_strided_row_section_mask
  use iso_fortran_env, only: int8
  implicit none
  integer(int8) :: a(3,4)
  integer(int8), allocatable :: packed(:)

  a = reshape([integer(int8) :: 10, 2, -3, -4, 6, -6, 7, -8, 9, 0, 1, 20], [3,4])
  packed = pack(a(1, :), a(1, :) > 0_int8)
  if (size(packed) /= 2) error stop 1
  if (packed(1) /= 10_int8) error stop 2
  if (packed(2) /= 7_int8) error stop 3
  print *, 'ok'
end program pack_strided_row_section_mask
