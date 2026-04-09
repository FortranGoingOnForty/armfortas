! Test FMADD/FMSUB/FNMSUB peephole fusion (O2+).
! Results must match at all optimization levels — correctness invariant.
program fma_peephole
  implicit none
  real :: a, b, c, r
  double precision :: da, db, dc, dr

  a = 2.0
  b = 3.0
  c = 4.0

  ! a*b + c = 10.0
  r = a * b + c
  print *, r

  ! c + a*b = 10.0  (commuted)
  r = c + a * b
  print *, r

  ! c - a*b = -2.0
  r = c - a * b
  print *, r

  ! a*b - c = 2.0
  r = a * b - c
  print *, r

  ! Double precision
  da = 1.5d0
  db = 2.0d0
  dc = 0.5d0

  ! da*db + dc = 3.5
  dr = da * db + dc
  print *, dr

  ! dc - da*db = -2.5
  dr = dc - da * db
  print *, dr
end program fma_peephole
! CHECK: 1.0000000E1
! CHECK: 1.0000000E1
! CHECK: -2.0000000E0
! CHECK: 2.0000000E0
! CHECK: 3.5
! CHECK: -2.5
