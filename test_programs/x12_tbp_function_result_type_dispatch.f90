! Regression: a type-bound function used as a generic actual must be
! typed by its RESULT variable, not an arbitrary local. version_t%s()
! returns character(len=:) but has integer/character locals too;
! function_scope_result_type_info returned the first non-argument
! variable in HashMap order, so the call's type came out Integer some
! runs and Character others (nondeterministic), and the generic
! `set_string`/`g` dispatch matched the wrong specific or none. Surfaced
! building fpm (dependency_config_t dump: set_string(..., v%s(), ...)).
! x12.
!
! CHECK: char:v7
module m
  implicit none
  type :: ver
    integer :: major = 7
  contains
    procedure :: s
  end type
  interface g
    module procedure g_char
    module procedure g_int
  end interface
contains
  function s(self) result(string)
    class(ver), intent(in) :: self
    ! Locals declared before the result is populated; different types,
    ! so a "first variable" heuristic would mistype the call. This is
    ! the whole point of the fixture — the mistyping is driven by the
    ! DECLARATIONS, not the body.
    integer :: ii
    integer, parameter :: bufsize = 64
    character(len=bufsize) :: buffer
    character(len=:), allocatable :: string
    ! Reference self, but keep the output independent of its value: a
    ! default-init component read (self%major) miscompiles to 0 at -O2
    ! on arm64, which is a separate bug and would make this assertion
    ! flap by target. The result-type-dispatch check doesn't need the
    ! value, so pin the string to a constant.
    ii = self%major
    buffer = 'v7'
    string = trim(buffer)
  end function
  subroutine g_char(key, val)
    character(*), intent(in) :: key, val
    write(*, '(a,a)') key, val
  end subroutine
  subroutine g_int(key, val)
    character(*), intent(in) :: key
    integer, intent(in) :: val
    write(*, '(a,i0)') key, val
  end subroutine
  subroutine dump(v)
    type(ver), intent(in) :: v
    call g('char:', v%s())
  end subroutine
end module
program p
  use m
  implicit none
  type(ver) :: v
  call dump(v)
end program p
