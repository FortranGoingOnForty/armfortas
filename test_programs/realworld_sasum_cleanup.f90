! stdlib BLAS sasum-inspired cleanup + chunked reduction loop.
! CHECK: 29
! IR_NOT: rt_call @__afs_check_bounds
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
program realworld_sasum_cleanup
  implicit none
  integer, parameter :: n = 10
  integer :: sx(n), stemp, i, m, mp1

  sx(1) = -5
  sx(2) = 4
  sx(3) = -3
  sx(4) = 2
  sx(5) = -1
  sx(6) = 0
  sx(7) = 1
  sx(8) = -2
  sx(9) = 3
  sx(10) = -8

  stemp = 0
  m = mod(n, 6)
  if (m /= 0) then
    do i = 1, m
      stemp = stemp + abs(sx(i))
    end do
  end if

  if (n < 6) then
    print *, stemp
    stop
  end if

  mp1 = m + 1
  do i = mp1, n, 6
    stemp = stemp + abs(sx(i)) + abs(sx(i + 1)) + abs(sx(i + 2)) + &
      abs(sx(i + 3)) + abs(sx(i + 4)) + abs(sx(i + 5))
  end do

  print *, stemp
end program realworld_sasum_cleanup
