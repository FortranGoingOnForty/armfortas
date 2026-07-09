program ar12_unimplemented_parity
  implicit none
  logical :: mask(4) = [.true., .false., .true., .true.]

  print *, parity(mask)
end program ar12_unimplemented_parity
! ERROR_EXPECTED: intrinsic 'parity' is recognized but not implemented
