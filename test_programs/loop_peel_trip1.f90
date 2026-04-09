! Edge case: loop peeling with trip count = 1.
! The peeled iteration is the ONLY iteration.
program loop_peel_trip1
  implicit none
  integer :: a(1), i
  do i = 1, 1
    if (i == 1) then
      a(i) = 42
    else
      a(i) = 0
    end if
  end do
  print *, a(1)
end program
! CHECK: 42
