! A loop-carried producer/consumer dependence can cross array bases.
! Distribution must preserve the per-iteration statement schedule.
program fission_cross_array_dependence
  implicit none
  integer :: a(5), b(5), i

  a = -99
  b = 0
  b(1) = 5
  do i = 2, 5
    if (i >= 2) then
      a(i) = b(i - 1)
      b(i) = a(i) + 1
    end if
  end do

  print *, a
  print *, b
end program
! CHECK: -99 5 6 7 8
! CHECK: 5 6 7 8 9
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
