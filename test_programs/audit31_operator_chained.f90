! Chained user-defined operator: `a + b + c` resolves the inner
! `a + b` to a vec_add call returning a derived type, then the
! outer `+ c` must dispatch through the same operator interface.
! Audit-level: catches the case where the BinaryOp lowering's
! intermediate result type prevents the outer overload from
! matching (it should match because the result is still a vec).
! CHECK: 6.0000000E0
module chain_op
  implicit none
  type :: vec
    real :: x
  end type
  interface operator(+)
    module procedure vec_add
  end interface
contains
  type(vec) function vec_add(a, b)
    type(vec), intent(in) :: a, b
    vec_add%x = a%x + b%x
  end function
end module
program t
  use chain_op
  implicit none
  type(vec) :: a, b, c, r
  a%x = 1.0
  b%x = 2.0
  c%x = 3.0
  r = a + b + c
  print *, r%x
end program
