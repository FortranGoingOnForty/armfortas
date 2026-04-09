! Test loop peeling: first-iteration branch elimination.
! The loop has a special case for i=1 that should be peeled out,
! leaving the remaining loop with a simplified body.
program loop_peel
  implicit none
  integer :: a(10), i
  do i = 1, 10
    if (i == 1) then
      a(i) = 0
    else
      a(i) = a(i-1) + 1
    end if
  end do
  print *, a(1)
  print *, a(5)
  print *, a(10)
end program
! CHECK: 0
! CHECK: 4
! CHECK: 9
