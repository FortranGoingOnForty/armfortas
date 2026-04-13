! Multi-file: three-level module dependency chain.
! MULTIFILE_LINK: base.f90 middle.f90 main.f90
! CHECK: 42
!--- file: base.f90
module base_mod
  implicit none
  integer, parameter :: BASE_VAL = 40
end module
!--- file: middle.f90
module middle_mod
  use base_mod
  implicit none
  integer, parameter :: MID_VAL = BASE_VAL + 2
end module
!--- file: main.f90
program p
  use middle_mod
  implicit none
  print *, MID_VAL
end program
