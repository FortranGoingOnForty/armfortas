! Audit #6 probe — USE ONLY filter walker covers complex
! literal real/imag arms via the cmplx() intrinsic. The walker
! reaches the imag argument through FunctionCall args.
!
! ERROR_EXPECTED: hidden
module audit6_filter_cmplx_mod
  integer :: visible = 1
  real :: hidden = 999.0
end module audit6_filter_cmplx_mod

program audit6_filter_complex_literal
  use audit6_filter_cmplx_mod, only: visible
  complex :: z
  z = cmplx(1.0, hidden)
  print *, z
end program
