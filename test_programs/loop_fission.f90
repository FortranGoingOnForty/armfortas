! Test loop fission: split a loop with two independent groups.
! Group A writes to a (reads b), Group B writes to c (reads d).
! No cross-group deps — fission should split into two loops.
program loop_fission
  implicit none
  integer :: a(10), b(10), c(10), d(10), i
  do i = 1, 10
    b(i) = i
    d(i) = i * 2
  end do
  do i = 1, 10
    a(i) = b(i) + 1
    c(i) = d(i) * 3
  end do
  print *, a(1), a(10)
  print *, c(1), c(10)
end program
! CHECK: 2 11
! CHECK: 6 60
