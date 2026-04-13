! Multi-file: module with allocatable array across TUs.
! MULTIFILE_LINK: mod.f90 main.f90
! CHECK: 10 20 30
!--- file: mod.f90
module arr_mod
  implicit none
  integer, allocatable :: buf(:)
contains
  subroutine init()
    allocate(buf(3))
    buf(1) = 10; buf(2) = 20; buf(3) = 30
  end subroutine
end module
!--- file: main.f90
program p
  use arr_mod
  implicit none
  call init()
  print *, buf(1), buf(2), buf(3)
end program
