! CHECK: ok
! IR_CHECK: call @afs_array_sum_real8_dim_mask
! IR_CHECK: call @afs_array_count_logical_dim
! IR_NOT: call @afs_array_count_logical(
! REPRO_CHECK: run
program stdlib_count_dim_expr_mask
  use, intrinsic :: iso_fortran_env, only: int8, real64
  implicit none

  integer(int8), parameter :: d1(18) = [-10_int8, 2_int8, 3_int8, 4_int8, -6_int8, 6_int8, &
       -7_int8, 8_int8, 9_int8, 4_int8, 1_int8, -20_int8, 9_int8, 10_int8, 14_int8, &
       15_int8, 40_int8, 30_int8]
  integer(int8) :: d2(3,6)
  real(real64) :: sums(6), means(6)
  integer :: counts(6)

  d2 = reshape(d1, [3,6])
  sums = sum(real(d2, real64), 1, d2 > 0_int8)
  counts = count(d2 > 0_int8, 1)
  means = sums / real(counts, real64)

  if (any(abs(sums - [5.0_real64, 10.0_real64, 17.0_real64, 5.0_real64, &
       33.0_real64, 85.0_real64]) > 1.0e-10_real64)) error stop 1
  if (any(counts /= [2, 2, 2, 2, 3, 3])) error stop 2
  if (any(abs(means - [2.5_real64, 5.0_real64, 8.5_real64, 2.5_real64, &
       11.0_real64, 85.0_real64 / 3.0_real64]) > 1.0e-10_real64)) error stop 3

  print *, 'ok'
end program
