! Multi-file: USE rename across TUs.
! MULTIFILE_LINK: mod.f90 main.f90
! CHECK: 99
!--- file: mod.f90
module rename_mod
  implicit none
  integer :: original = 99
end module
!--- file: main.f90
program p
  use rename_mod, renamed => original
  implicit none
  print *, renamed
end program
