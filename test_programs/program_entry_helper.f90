! CHECK: 99
! ASM_CHECK: _main:
! ASM_CHECK: bl ___prog_program_entry_helper
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
subroutine set_value(x)
  implicit none
  integer, intent(out) :: x
  x = 99
end subroutine set_value

program program_entry_helper
  implicit none
  integer :: x
  call set_value(x)
  print *, x
end program program_entry_helper
