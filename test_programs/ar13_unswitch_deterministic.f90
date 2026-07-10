! Loop unswitching must choose invariant conditionals deterministically.
!
! REPRO_CHECK: asm
! CHECK: x|
! CHECK: x|
! CHECK: x
program ar13_unswitch_deterministic
  implicit none
  call walk('x', .false., 2)
  call walk('x', .true., 1)

contains
  subroutine walk(prefix, is_last, n)
    character(len=*), intent(in) :: prefix
    logical, intent(in) :: is_last
    integer, intent(in) :: n
    character(len=:), allocatable :: new_prefix
    integer :: i

    do i = 1, n
      if (is_last) then
        new_prefix = prefix // '    '
      else
        new_prefix = prefix // '|   '
      end if
      print '(a)', new_prefix
    end do
  end subroutine walk
end program ar13_unswitch_deterministic
