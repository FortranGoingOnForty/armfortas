! Multi-file: basic module variable and subroutine across TUs.
! MULTIFILE_LINK: mod.f90 main.f90
! CHECK: 3
!--- file: mod.f90
module m
  implicit none
  integer :: counter = 0
contains
  subroutine bump()
    counter = counter + 1
  end subroutine
  integer function get() result(r)
    r = counter
  end function
end module
!--- file: main.f90
program p
  use m
  implicit none
  call bump(); call bump(); call bump()
  print *, get()
end program
