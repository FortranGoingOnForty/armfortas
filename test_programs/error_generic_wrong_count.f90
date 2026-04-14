! Calling a generic with the wrong argument count must be rejected
! at compile time, not silently dispatched to a specific that then
! mismatches the actual ABI.
! ERROR_EXPECTED: no specific procedure
module m
  interface add
    module procedure add2
  end interface
contains
  integer function add2(a, b)
    integer, intent(in) :: a, b
    add2 = a + b
  end function
end module
program t
  use m
  print *, add(1, 2, 3)
end program
