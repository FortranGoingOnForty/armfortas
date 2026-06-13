! l04: a user procedure named SPLIT shadows the intrinsic — intrinsic
! resolution must lose to an explicit local/contained definition (fpm
! ships its own split). The contained function is called, not the
! F2023 string subroutine.
! FLAGS: --std=f2023
! CHECK: 6
! CHECK: ok
program l04_split_user_shadow
  implicit none
  integer :: r
  r = split(3)
  write(*, '(I0)') r
  write(*, '(A)') 'ok'
contains
  integer function split(n)
    integer, intent(in) :: n
    split = n * 2
  end function
end program l04_split_user_shadow
