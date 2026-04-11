! BLAS-inspired axpy + reduction shape.
! CHECK: 6 6 6 216
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_axpy_reduce
  implicit none
  integer, parameter :: n = 8
  integer :: x(n), y(n)
  integer :: i, weighted

  x = [1, 3, 5, 7, 9, 11, 13, 15]
  y = [5, 3, 1, -1, -3, -5, -7, -9]

  do i = 1, n
    y(i) = x(i) + y(i)
  end do

  weighted = 0
  do i = 1, n
    weighted = weighted + i * y(i)
  end do

  print *, y(1), y(2), y(4), weighted
end program realworld_axpy_reduce
