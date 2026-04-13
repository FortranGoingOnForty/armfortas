! Elemental procedure argument with DIMENSION is not scalar.
! ERROR_EXPECTED: must be scalar
program t
  implicit none
contains
  elemental real function scale(a)
    real, intent(in), dimension(3) :: a
    scale = a(1)
  end function
end program
