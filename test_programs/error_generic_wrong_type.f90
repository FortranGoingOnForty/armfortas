! Calling a generic with argument types that match no specific must
! be rejected. Previously we silently picked the first (or only)
! specific and passed mis-typed values through.
! ERROR_EXPECTED: no specific procedure
module m
  interface add
    module procedure add_i
  end interface
contains
  integer function add_i(a, b)
    integer, intent(in) :: a, b
    add_i = a + b
  end function
end module
program t
  use m
  print *, add(1.5, 2.5)
end program
