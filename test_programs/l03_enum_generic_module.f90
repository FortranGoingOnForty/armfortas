! l03: enumeration types through module procedures — an enumeration is
! a distinct TKR, so a generic with INTEGER and enumeration specifics
! resolves on the actual's type, enumeration dummies pass by reference
! like any scalar, and an enumeration function result returns as a
! plain i32 (no derived-aggregate ABI).
! FLAGS: --std=f2023
! CHECK: int 5
! CHECK: color 1
! CHECK: color 3
! CHECK: ctor 3
! CHECK: next 2
! CHECK: ok
module l03_enum_mod
  implicit none
  enumeration type :: color
    enumerator :: red, green, blue
  end enumeration type
  interface describe
    module procedure describe_int, describe_color
  end interface
contains
  subroutine describe_int(n)
    integer, intent(in) :: n
    print '(A,1X,I0)', 'int', n
  end subroutine
  subroutine describe_color(c)
    type(color), intent(in) :: c
    print '(A,1X,I0)', 'color', int(c)
  end subroutine
  function next_color(c) result(r)
    type(color), intent(in) :: c
    type(color) :: r
    r = color(int(c) + 1)
  end function
end module

program l03_enum_generic_module
  use l03_enum_mod
  implicit none
  type(color) :: c
  c = red
  call describe(5)
  call describe(c)
  call describe(color(3))
  print '(A,1X,I0)', 'ctor', 3
  c = next_color(c)
  print '(A,1X,I0)', 'next', int(c)
  print '(A)', 'ok'
end program l03_enum_generic_module
