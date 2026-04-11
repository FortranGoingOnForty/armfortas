! two-stage in-place prefix kernel with carried state between loops.
! CHECK: 1 3 36 120
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_inplace_prefix
  implicit none
  integer, parameter :: n = 8
  integer :: a(n), i, total

  do i = 1, n
    a(i) = i
  end do

  do i = 2, n
    a(i) = a(i) + a(i - 1)
  end do

  total = 0
  do i = 1, n
    total = total + a(i)
  end do

  print *, a(1), a(2), a(8), total
end program realworld_inplace_prefix
