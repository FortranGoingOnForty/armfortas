! L-tail arity gate on CALL statements: random_number requires its
! harvest argument (F2023 16.9.167).
! ERROR_EXPECTED: intrinsic 'random_number' takes 1 argument, got 0
program ltail_intrinsic_arity_reject_call
  implicit none
  call random_number()
end program ltail_intrinsic_arity_reject_call
