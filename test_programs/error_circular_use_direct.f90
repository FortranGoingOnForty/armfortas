! Direct circular module dependency: A uses B, B uses A.
! This is a multi-file test that should fail at compile time.
! MULTIFILE_LINK: a.f90 b.f90 main.f90
! ERROR_EXPECTED: not found
!--- file: a.f90
module mod_a
  use mod_b
  implicit none
  integer :: val_a = 1
end module
!--- file: b.f90
module mod_b
  use mod_a
  implicit none
  integer :: val_b = 2
end module
!--- file: main.f90
program p
  use mod_a
  print *, val_a
end program
