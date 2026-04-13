! Multi-file: PARAMETER constants across TUs.
! MULTIFILE_LINK: mod.f90 main.f90
! CHECK: 1024
! CHECK: 512
!--- file: mod.f90
module consts
  implicit none
  integer, parameter :: MAX_N = 1024
  integer, parameter :: HALF = MAX_N / 2
end module
!--- file: main.f90
program p
  use consts
  implicit none
  print *, MAX_N
  print *, HALF
end program
