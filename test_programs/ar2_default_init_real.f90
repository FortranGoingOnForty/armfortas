! CHECK: local 7 T xy 15 225
! CHECK: save 7 T xy 15 225
! CHECK: module 7 T xy 15 225
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
module ar2_default_init_real_mod
  implicit none

  type :: sample
    integer :: i = 7
    logical :: l = .true.
    character(len=4) :: c = "xy"
    real :: r4 = 1.5
    real(8) :: r8 = 2.25_8
  end type sample

  type(sample) :: mod_s
contains
  subroutine show(tag, s)
    character(*), intent(in) :: tag
    type(sample), intent(in) :: s

    print '(a,1x,i0,1x,l1,1x,a,1x,i0,1x,i0)', &
      tag, s%i, s%l, trim(s%c), nint(s%r4 * 10.0), nint(s%r8 * 100.0_8)
  end subroutine show

  subroutine show_save()
    type(sample), save :: save_s

    call show("save", save_s)
  end subroutine show_save
end module ar2_default_init_real_mod

program ar2_default_init_real
  use ar2_default_init_real_mod, only: mod_s, sample, show, show_save
  implicit none

  type(sample) :: local_s

  call show("local", local_s)
  call show_save()
  call show("module", mod_s)
end program ar2_default_init_real
