! INTERFACE OPERATOR(+) dispatches `a + b` to a user-defined
! specific when the operand types are derived types. The default
! arithmetic path would ICE on Ptr(Array(i8, N)) + Ptr(Array(...)).
! CHECK: 3.0000000E0
module opmod
  implicit none
  type :: vec
    real :: x
  end type
  interface operator(+)
    module procedure vec_add
  end interface
contains
  real function vec_add(a, b)
    type(vec), intent(in) :: a, b
    vec_add = a%x + b%x
  end function
end module
program t
  use opmod
  implicit none
  type(vec) :: a, b
  a%x = 1.0
  b%x = 2.0
  print *, a + b
end program
