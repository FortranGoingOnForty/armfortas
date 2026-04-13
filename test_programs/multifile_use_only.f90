! Multi-file: USE ONLY filtering across TUs.
! MULTIFILE_LINK: mod.f90 main.f90
! CHECK: 20
!--- file: mod.f90
module big_mod
  implicit none
  integer :: alpha = 10
  integer :: beta = 20
  integer :: gamma = 30
end module
!--- file: main.f90
program p
  use big_mod, only: beta
  implicit none
  print *, beta
end program
