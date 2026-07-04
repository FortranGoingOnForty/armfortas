! L-tail arity gate: an intrinsic reference with too few arguments is
! a compile error (F2023 16.9; previously compiled silently and
! produced garbage — noted_items l04 find).
! ERROR_EXPECTED: intrinsic 'atan2' takes 2 arguments, got 1
program ltail_intrinsic_arity_reject_few
  implicit none
  real :: x
  x = atan2(1.0)
  print *, x
end program ltail_intrinsic_arity_reject_few
