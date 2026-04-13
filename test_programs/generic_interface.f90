! Generic interface resolution: dispatch by argument type.
! CHECK: 3
! CHECK: 4.0
module math_ops
  implicit none
  interface add
    module procedure add_int, add_real
  end interface
contains
  integer function add_int(a, b)
    integer, intent(in) :: a, b
    add_int = a + b
  end function
  real function add_real(a, b)
    real, intent(in) :: a, b
    add_real = a + b
  end function
end module
program t
  use math_ops
  implicit none
  print *, add(1, 2)
  print *, add(1.5, 2.5)
end program
