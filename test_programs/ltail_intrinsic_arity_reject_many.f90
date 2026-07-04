! L-tail arity gate: too many arguments to an intrinsic is a compile
! error. len takes the string and an optional kind (F2023 16.9.144).
! ERROR_EXPECTED: intrinsic 'len' takes 1 to 2 arguments, got 3
program ltail_intrinsic_arity_reject_many
  implicit none
  character(len=8) :: s
  integer :: n
  s = 'hello'
  n = len(s, 4, 1)
  print *, n
end program ltail_intrinsic_arity_reject_many
