! Test function inlining for PURE contained functions.
! At O1+, the call to double_it should be replaced by the inlined body.
program inline_pure
  implicit none
  integer :: x
  x = double_it(5)
  print *, x
  x = double_it(double_it(3))
  print *, x
contains
  pure function double_it(n) result(r)
    integer, intent(in) :: n
    integer :: r
    r = n * 2
  end function
end program
! CHECK: 10
! CHECK: 12
