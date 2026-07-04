! L-tail: CALLing a function intrinsic is a compile error.
! ERROR_EXPECTED: intrinsic 'sqrt' is a function; reference it in an expression, not a CALL
program ltail_intrinsic_form_reject_callfn
  implicit none
  real :: x
  x = 2.0
  call sqrt(x)
end program ltail_intrinsic_form_reject_callfn
