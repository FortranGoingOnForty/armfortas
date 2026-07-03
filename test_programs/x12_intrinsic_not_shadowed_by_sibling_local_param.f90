! Regression (fpm m_cli2::a2i): a `parameter :: int` (character) local to
! one procedure must NOT shadow the intrinsic INT in a sibling procedure.
! armfortas resolved `int(x)` in `a2i` via a scope-blind symbol scan,
! found the leaked constant, and lowered the call as a substring of it —
! emitting `movslq %xmm` (a float sign-extend the assembler rejects).
! The intrinsic must win here; gfortran gives 3.
module m_shadow
  implicit none
contains
  logical function is_name(line)
    character(len=*), parameter  :: int = '0123456789'
    character(len=*), intent(in) :: line
    is_name = verify(trim(line), int) == 0
  end function
  subroutine a2i(valu)
    integer, intent(out) :: valu
    double precision :: valu8
    valu8 = 3.7d0
    valu = int(valu8)   ! intrinsic INT -> 3, not a substring of `int`
  end subroutine
end module
program p
  use m_shadow
  implicit none
  integer :: v
  call a2i(v)
  write(*, '(a,i0)') 'v=', v
  write(*, '(a,l1)') 'is_name=', is_name('123')
  if (v /= 3) error stop 1
  if (.not. is_name('123')) error stop 2
end program p
! CHECK: v=3
! CHECK: is_name=T
