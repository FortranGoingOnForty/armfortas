! Operator interface function must have 1 or 2 arguments.
! ERROR_EXPECTED: 1 or 2 arguments
program t
  implicit none
  interface operator(+)
    function add_fn(a, b, c) result(r)
      integer, intent(in) :: a, b, c
      integer :: r
    end function
  end interface
end program
