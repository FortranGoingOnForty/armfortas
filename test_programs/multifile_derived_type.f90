! Multi-file: module with derived type across TUs.
! MULTIFILE_LINK: mod.f90 main.f90
! CHECK: 1.5
! CHECK: 2.5
!--- file: mod.f90
module dt_mod
  implicit none
  type :: point
    real :: x, y
  end type
contains
  subroutine set_pt(p, a, b)
    type(point), intent(out) :: p
    real, intent(in) :: a, b
    p%x = a; p%y = b
  end subroutine
end module
!--- file: main.f90
program p
  use dt_mod
  implicit none
  type(point) :: pt
  call set_pt(pt, 1.5, 2.5)
  print *, pt%x
  print *, pt%y
end program
