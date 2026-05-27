! CHECK: ok
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program matmul_transpose_inline_rank2_array_expr
  implicit none

  real :: a(3,3), l(3,3), recon(3,3), direct(3,3)
  real :: tol

  tol = 100.0*sqrt(epsilon(0.0))

  a(1,:) = [6.0, 15.0, 55.0]
  a(2,:) = [15.0, 55.0, 225.0]
  a(3,:) = [55.0, 225.0, 979.0]

  l(1,:) = [2.4494898, 0.0000000, 0.0000000]
  l(2,:) = [6.1237240, 4.1833005, 0.0000000]
  l(3,:) = [22.4536550, 20.9165020, 6.1100993]

  recon = matmul(l, transpose(l))
  direct = a - matmul(l, transpose(l))

  if (.not. all(abs(a - recon) < tol)) error stop 1
  if (.not. all(abs(direct) < tol)) error stop 2
  if (.not. all(abs(a - matmul(l, transpose(l))) < tol)) error stop 3

  print *, "ok"
end program matmul_transpose_inline_rank2_array_expr
