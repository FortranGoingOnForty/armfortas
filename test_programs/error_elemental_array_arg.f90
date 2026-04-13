! Elemental procedure arguments must be scalar.
! ERROR_EXPECTED: must be scalar
program t
  implicit none
contains
  elemental integer function bad(a)
    integer, intent(in) :: a(:)
    bad = a(1)
  end function
end program
