! CHECK: ok
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program complex_vector_subscript_section_expr
  implicit none
  integer, parameter :: dp = kind(0.0d0)
  integer, parameter :: m = 5, n = 4
  complex(dp) :: a(m,n), b(m,n), c(m,n)
  integer :: pivots(n)
  integer :: i, j
  real(dp) :: tol, max_err

  pivots = [4, 2, 3, 1]
  tol = 1.0d-12

  do j = 1, n
    do i = 1, m
      a(i,j) = cmplx(real(i + 10*j, dp), real(2*i - j, dp), kind=dp)
    end do
  end do

  do j = 1, n
    do i = 1, m
      b(i,j) = a(i,pivots(j))
    end do
  end do

  c = a(:, pivots)
  max_err = 0.0_dp
  do j = 1, n
    do i = 1, m
      max_err = max(max_err, abs(c(i,j) - b(i,j)))
    end do
  end do

  if (max_err >= tol) error stop 1
  if (.not. all(abs(c - b) < tol)) error stop 2
  if (.not. all(abs(a(:, pivots) - b) < tol)) error stop 3
  print *, 'ok'
end program complex_vector_subscript_section_expr
