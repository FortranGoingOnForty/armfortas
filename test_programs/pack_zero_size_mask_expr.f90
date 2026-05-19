! CHECK: ok
! IR_CHECK: call @afs_array_pack
! REPRO_CHECK: run
! PHASE_TRIANGULATE: ir|asm|obj|repro
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program pack_zero_size_mask_expr
  use iso_fortran_env, only: int8
  implicit none
  integer(int8), allocatable :: d0(:)
  integer(int8), allocatable :: packed(:)

  allocate(d0(0))
  packed = pack(d0, d0 > 0_int8)
  if (size(packed) /= 0) error stop 1
  print *, 'ok'
end program pack_zero_size_mask_expr
