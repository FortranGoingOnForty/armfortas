! Indirect circular dependency: A -> B -> C -> A.
! MULTIFILE_LINK: a.f90 b.f90 c.f90 main.f90
! ERROR_EXPECTED: not found
!--- file: a.f90
module mod_a
  use mod_c
  implicit none
  integer :: val_a = 1
end module
!--- file: b.f90
module mod_b
  use mod_a
  implicit none
  integer :: val_b = 2
end module
!--- file: c.f90
module mod_c
  use mod_b
  implicit none
  integer :: val_c = 3
end module
!--- file: main.f90
program p
  use mod_a
  print *, val_a
end program
