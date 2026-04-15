! audit31 harvest: defined operator(+) on a derived type exported via
! a module.  Sprint 31 #465 / #477 fixed the single-TU resolution;
! this pins the canonical form that links the module defining the
! operator against the program that USEs it.  Exercises generic
! dispatch on '+' AND derived-type function result lowering AND
! struct-by-value argument passing all in one small program.
! CHECK: 4.0000000E0     6.0000000E0
module audit31_op_single_mod
  implicit none
  type :: v2
    real :: x = 0.0, y = 0.0
  end type
  interface operator(+)
    module procedure add_v2
  end interface
contains
  function add_v2(a, b) result(r)
    type(v2), intent(in) :: a, b
    type(v2) :: r
    r%x = a%x + b%x
    r%y = a%y + b%y
  end function
end module

program audit31_op_single
  use audit31_op_single_mod
  implicit none
  type(v2) :: a, b, c
  a%x = 1.0; a%y = 2.0
  b%x = 3.0; b%y = 4.0
  c = a + b
  print *, c%x, c%y
end program
