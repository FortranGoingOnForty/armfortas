! stdlib-inspired tridiagonal sparse matvec shape.
! CHECK: 53 157 221 994
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_tridiag_spmv
  implicit none
  integer, parameter :: n = 6
  integer :: dl(n-1), dv(n), du(n-1), x(n), y(n)
  integer :: i, total

  dl = [1, 2, 3, 4, 5]
  dv = [10, 11, 12, 13, 14, 15]
  du = [6, 7, 8, 9, 10]
  x = [2, 1, 3, 5, 4, 6]
  y = [1, 1, 1, 1, 1, 1]

  y(1) = 2 * (dv(1) * x(1) + du(1) * x(2)) + y(1)
  do i = 2, n - 1
    y(i) = 2 * (dl(i-1) * x(i-1) + dv(i) * x(i) + du(i) * x(i+1)) + y(i)
  end do
  y(n) = 2 * (dl(n-1) * x(n-1) + dv(n) * x(n)) + y(n)

  total = 0
  do i = 1, n
    total = total + y(i)
  end do

  print *, y(1), y(3), y(6), total
end program realworld_tridiag_spmv
