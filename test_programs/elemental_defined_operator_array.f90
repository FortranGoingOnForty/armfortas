! Audit C4: an ELEMENTAL defined operator applied to array operands must be
! applied element-wise, not evaluated once on the first element and broadcast.
! `res = a + one` (a is an array, + is a user operator) printed `11 11 11`
! instead of `11 12 13`: the array-expression path had no defined-operator
! case, so the RHS was evaluated as a scalar and broadcast across res. The
! fix scalarizes `res = <elemental-op tree>` into a per-element assignment,
! recursing through nested operators so `a + c + one` works too.
module m4mod
  type :: vec
    integer :: v
  end type
  interface operator(+)
    module procedure vadd
  end interface
  interface operator(*)
    module procedure vmul
  end interface
contains
  elemental function vadd(a, b) result(r)
    type(vec), intent(in) :: a, b
    type(vec) :: r
    r%v = a%v + b%v
  end function
  elemental function vmul(a, b) result(r)
    type(vec), intent(in) :: a, b
    type(vec) :: r
    r%v = a%v*b%v
  end function
end module

program elemental_defined_operator_array
  use m4mod
  type(vec) :: a(4), c(4), one, res(4)
  integer :: i

  do i = 1, 4
    a(i)%v = i
    c(i)%v = 10*i
  end do
  one%v = 100

  ! scalar operand on the right (the audit reproducer shape)
  res = a + one
  print '(A,4I5)', 'AO', res%v
  ! CHECK: AO  101  102  103  104

  ! scalar operand on the left
  res = one + a
  print '(A,4I5)', 'OA', res%v
  ! CHECK: OA  101  102  103  104

  ! both operands arrays
  res = a + c
  print '(A,4I5)', 'AC', res%v
  ! CHECK: AC   11   22   33   44

  ! a different elemental operator
  res = a*c
  print '(A,4I5)', 'MU', res%v
  ! CHECK: MU   10   40   90  160

  ! nested chain: (a + c) + one
  res = a + c + one
  print '(A,4I5)', 'NS', res%v
  ! CHECK: NS  111  122  133  144
end program
