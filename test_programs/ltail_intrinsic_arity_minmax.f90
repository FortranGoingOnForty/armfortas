! L-tail arity gate: MAX/MIN are unbounded above but need two
! arguments; long argument lists stay legal.
! ERROR_EXPECTED: intrinsic 'max' takes at least 2 arguments, got 1
program ltail_intrinsic_arity_minmax
  implicit none
  integer :: n
  n = max(1, 2, 3, 4, 5, 6)
  n = max(1)
  print *, n
end program ltail_intrinsic_arity_minmax
