! Regression: a local data object shadows a same-named generic procedure.
! Here `new` is a character(len=*) dummy, so `new(:n)` is a substring; a
! generic interface `new` in another module must not capture it. The
! callable-callee check claimed any visible named interface made the name
! callable, so the substring path was skipped and dispatch was attempted
! against the unrelated generic. Surfaced building fpm: M_strings
! `substitute(targetline, old, new, ...)` does `new(:len_new)` while
! tomlf exposes a generic `new`. x12.
!
! CHECK: result=hel
module gen_mod
  implicit none
  type :: foo
    integer :: x
  end type
  interface new
    module procedure new_foo
  end interface
contains
  subroutine new_foo(self)
    type(foo), intent(out) :: self
    self%x = 0
  end subroutine
end module

module str_mod
  implicit none
contains
  subroutine clip(new, res)
    character(len=*), intent(in) :: new
    character(len=*), intent(out) :: res
    integer :: n
    n = 3
    res = new(:n)
  end subroutine
end module

program t
  use str_mod
  use gen_mod
  implicit none
  character(len=10) :: r
  type(foo) :: f
  call new(f)
  call clip('hello', r)
  write(*, '(a,a)') 'result=', trim(r)
end program t
