! L-tail: an intrinsic SUBROUTINE referenced in function position is
! a compile error (used to compile and die at link with
! "undefined symbol: system_clock" — noted_items l04 find).
! ERROR_EXPECTED: intrinsic 'system_clock' is a subroutine, not a function
program ltail_intrinsic_form_reject_subfn
  implicit none
  integer :: r
  r = system_clock()
  print *, r
end program ltail_intrinsic_form_reject_subfn
