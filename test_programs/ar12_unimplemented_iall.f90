program ar12_unimplemented_iall
  implicit none
  integer :: v(4) = [1, 2, 3, 4]

  print *, iall(v)
end program ar12_unimplemented_iall
! ERROR_EXPECTED: intrinsic 'iall' is recognized but not implemented
