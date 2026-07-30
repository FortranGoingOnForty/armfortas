! A USE-associated generic is visible only in the importing program unit.
! The main program's explicit interface for external function g must not be
! contaminated by an unrelated subroutine's USE of the generic with that name.
!
! CHECK: 42 101
! IR_CHECK: func @unrelated_importer(
! IR_CHECK: call @afs_modproc_ar43_generic_owner_module_g(
! IR_CHECK: func @g(
! IR_CHECK: func @__prog_ar43_unrelated_use_generic(
! IR_CHECK: call @g(
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
module ar43_generic_owner
  implicit none
  interface g
    module procedure module_g
  end interface g
contains
  integer function module_g(value)
    integer, intent(in) :: value
    module_g = value + 100
  end function module_g
end module ar43_generic_owner

subroutine unrelated_importer(answer)
  use ar43_generic_owner, only: g
  implicit none
  integer, intent(out) :: answer

  answer = g(1)
end subroutine unrelated_importer

integer function g(value)
  implicit none
  integer, intent(in) :: value
  g = value + 1
end function g

program ar43_unrelated_use_generic
  implicit none
  interface
    integer function g(value)
      integer, intent(in) :: value
    end function g
    subroutine unrelated_importer(answer)
      integer, intent(out) :: answer
    end subroutine unrelated_importer
  end interface
  integer :: answer, imported_answer

  call unrelated_importer(imported_answer)
  answer = g(41)
  print *, answer, imported_answer
  if (answer /= 42) error stop 1
  if (imported_answer /= 101) error stop 2
end program ar43_unrelated_use_generic
