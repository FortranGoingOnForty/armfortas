module m
  implicit none
  integer, parameter :: N = 42
  real, allocatable :: buf(:)
contains
  pure integer function f(x)
    integer, intent(in) :: x
    f = x * 2
  end function
end module
