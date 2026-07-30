! BLOCK-local USE resolution must preserve the parsed module nature.
! A same-named authored module must not capture an explicit INTRINSIC import,
! while NON_INTRINSIC and ordinary USE must still select the authored module.
!
! CHECK: 1 4 4 F T
! IR_CHECK: func @afs_modproc_iso_c_binding_c_associated(
! IR_CHECK: func @__prog_ar43_block_use_nature(
! IR_CHECK: call @afs_modproc_iso_c_binding_c_associated(
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module iso_fortran_env
  implicit none
  integer, parameter :: int8 = 4
end module iso_fortran_env

module iso_c_binding
  implicit none
contains
  logical function c_associated(value)
    integer, intent(in) :: value
    c_associated = value == 0
  end function c_associated
end module iso_c_binding

subroutine ar43_unrelated_use
  use, non_intrinsic :: iso_c_binding, only: c_associated
  implicit none
  if (.not. c_associated(0)) error stop 6
end subroutine ar43_unrelated_use

program ar43_block_use_nature
  implicit none
  integer :: intrinsic_value, nonintrinsic_value, normal_value
  logical :: intrinsic_callable, nonintrinsic_callable

  block
    use, intrinsic :: iso_fortran_env, only: int8
    intrinsic_value = int8
  end block

  block
    use, non_intrinsic :: iso_fortran_env, only: int8
    nonintrinsic_value = int8
  end block

  block
    use iso_fortran_env, only: int8
    normal_value = int8
  end block

  block
    use, intrinsic :: iso_c_binding, only: c_associated, c_null_ptr
    intrinsic_callable = c_associated(c_null_ptr)
  end block

  block
    use, non_intrinsic :: iso_c_binding, only: c_associated
    nonintrinsic_callable = c_associated(0)
  end block

  print *, intrinsic_value, nonintrinsic_value, normal_value, &
    intrinsic_callable, nonintrinsic_callable
  if (intrinsic_value /= 1) error stop 1
  if (nonintrinsic_value /= 4) error stop 2
  if (normal_value /= 4) error stop 3
  if (intrinsic_callable) error stop 4
  if (.not. nonintrinsic_callable) error stop 5
end program ar43_block_use_nature
