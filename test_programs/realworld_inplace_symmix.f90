! in-place symmetry sweep with transposed reads and writes.
! CHECK: 33 54 88 141
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_inplace_symmix
  implicit none
  integer, parameter :: n = 6
  integer :: a(n, n), i, j

  do j = 1, n
    do i = 1, n
      a(i, j) = 10 * i + j
    end do
  end do

  do i = 1, n
    do j = 1, n
      a(i, j) = a(i, j) + a(j, i)
    end do
  end do

  print *, a(1, 2), a(2, 1), a(3, 5), a(5, 3)
end program realworld_inplace_symmix
