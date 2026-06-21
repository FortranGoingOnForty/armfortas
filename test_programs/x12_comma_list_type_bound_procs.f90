! Regression: a comma-separated type-bound procedure binding
! (`procedure :: a, b, c`, F2018 R448) must bind every name, not just
! the first. The parser read one name after `::` and dropped the rest,
! so calling a later binding failed with "no specific type-bound
! procedure ... candidates: []". Surfaced building fpm:
! `fpm_global_settings` binds `procedure :: has_custom_location,
! full_path, path_to_config_folder_or_empty` and calls `%full_path()`.
! x12.
!
! CHECK: hl=T
! CHECK: len=3
! CHECK: fp=abc
module m
  implicit none
  type :: settings
    integer :: n = 3
  contains
    procedure :: has_loc, full_path
  end type
contains
  logical function has_loc(self)
    class(settings), intent(in) :: self
    ! Return a constant: this test isolates the comma-list binding
    ! mechanism (both names callable), not component init/read, which is
    ! optimization-sensitive on some targets.
    has_loc = .true.
  end function
  function full_path(self) result(r)
    class(settings), intent(in) :: self
    character(len=:), allocatable :: r
    r = 'abc'
  end function
  logical function probe(str)
    character(*), intent(in) :: str
    probe = len(str) > 0
  end function
  subroutine work(s)
    type(settings), intent(inout) :: s
    write(*, '(a,l1)') 'hl=', s%has_loc()
    write(*, '(a,i0)') 'len=', len(s%full_path())
    if (probe(s%full_path())) write(*, '(a,a)') 'fp=', s%full_path()
  end subroutine
end module
program p
  use m
  implicit none
  type(settings) :: s
  call work(s)
end program p
