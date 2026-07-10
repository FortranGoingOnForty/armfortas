! CHECK: dummy len= 26
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program ar9_char_ac_sibling_len
  implicit none

  call b()

contains
  subroutine a()
    character(len=10) :: ignored_path

    ignored_path = '0123456789'
  end subroutine a

  subroutine b()
    character(len=:), allocatable :: ignored_path

    ignored_path = 'abcdefghijklmnopqrstuvwxyz'
    call show([ignored_path])
  end subroutine b

  subroutine show(arr)
    character(len=*), intent(in) :: arr(:)

    if (len(arr) /= 26) error stop 1
    print '(a,1x,i0)', 'dummy len=', len(arr)
    print '(a)', 'ok'
  end subroutine show
end program ar9_char_ac_sibling_len
