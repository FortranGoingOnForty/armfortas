! Uninitialized module data reserves zero-fill storage instead of bytes in .data.
!
! CHECK: bss 4 7
! ASM_CHECK(x86_64-linux-gnu): .comm afs_mod_ar6_bss_module_data_a,800000,16
! ASM_CHECK(x86_64-freebsd): .comm afs_mod_ar6_bss_module_data_a,800000,16
! ASM_CHECK(arm64): .zerofill __DATA,__bss,_afs_mod_ar6_bss_module_data_a,800000,4
module ar6_bss_module_data
  implicit none
  real(8) :: a(100000)
  integer :: marker
contains
  subroutine touch()
    a(1) = 1.25d0
    a(size(a)) = 2.75d0
    marker = 7
  end subroutine touch
end module ar6_bss_module_data

program ar6_bss_module_data_main
  use ar6_bss_module_data
  implicit none
  call touch()
  print '(a,i0,1x,i0)', 'bss ', int(a(1) + a(size(a))), marker
end program ar6_bss_module_data_main
