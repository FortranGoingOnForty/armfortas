! CHECK: ok
! IR_CHECK: int_to_float
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program mixed_numeric_array_binary_exprs
  implicit none
  logical :: corrected
  real :: n(3)
  integer :: k(3)
  real :: denom(3), rev_denom(3), sumv(3), prodv(3), quotv(3)

  corrected = .true.
  n = [4.0, 4.0, 2.0]
  k = merge(1, 0, corrected .and. n > 0.0)

  denom = n - k
  rev_denom = k - n
  sumv = n + k
  prodv = n * k
  quotv = n / k

  if (any(k /= [1, 1, 1])) error stop 1
  if (any(abs(denom - [3.0, 3.0, 1.0]) > 1.0e-6)) error stop 2
  if (any(abs(rev_denom - [-3.0, -3.0, -1.0]) > 1.0e-6)) error stop 3
  if (any(abs(sumv - [5.0, 5.0, 3.0]) > 1.0e-6)) error stop 4
  if (any(abs(prodv - [4.0, 4.0, 2.0]) > 1.0e-6)) error stop 5
  if (any(abs(quotv - [4.0, 4.0, 2.0]) > 1.0e-6)) error stop 6

  print *, 'ok'
end program
