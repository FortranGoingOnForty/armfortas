program ofast_fast_math_reassoc
  implicit none
  real(8) :: x

  x = dble(command_argument_count()) + 1.0d0
  print *, nint((x + 10000000000000000.d0) - 10000000000000000.d0)
end program ofast_fast_math_reassoc
