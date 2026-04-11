! XFAIL: audit MODULE-HOST-1 (module procedure misses module global host association)
! CHECK: 99
module module_global_host_assoc_mod
  implicit none
  integer :: g = 0
contains
  subroutine bump()
    implicit none
    g = 99
  end subroutine bump
end module module_global_host_assoc_mod

program module_global_host_assoc
  use module_global_host_assoc_mod
  implicit none
  g = 1
  call bump()
  print *, g
end program module_global_host_assoc
