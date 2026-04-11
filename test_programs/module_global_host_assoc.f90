! Module procedure host association over module globals.
! CHECK: 99
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
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
