! Kind-suffix real literals (_dp, _8) must preserve full precision.
! Regression test: previously _dp parsed as f32 then widened to f64.
! CHECK: 1.234567890123457
! CHECK: 1.000000000000000E-2
program t
  implicit none
  integer, parameter :: dp = 8
  real(dp) :: x, y
  x = 1.234567890123456789_dp
  y = 0.1_dp * 0.1_dp
  print *, x
  print *, y
end program
