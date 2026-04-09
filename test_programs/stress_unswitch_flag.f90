! Stress test: loop unswitching with invariant conditional.
! The flag is set before the loop and never modified inside it,
! so the optimizer should hoist the conditional out of the loop.
program stress_unswitch_flag
  implicit none
  integer :: a(100), i, flag
  flag = 1
  do i = 1, 100
    if (flag > 0) then
      a(i) = i * 2
    else
      a(i) = i * 3
    end if
  end do
  print *, a(1)
  print *, a(50)
  print *, a(100)
end program
! CHECK: 2
! CHECK: 100
! CHECK: 200
