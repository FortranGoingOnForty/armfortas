! Logical NOT correctness: .not. must invert boolean values.
! CHECK: F
! CHECK: T
! CHECK: F
! CHECK: T
! CHECK: T
! CHECK: T
! CHECK: F
program t
  implicit none
  logical :: a
  integer :: x
  print *, .not. .true.
  print *, .not. .false.
  a = .true.
  print *, .not. a
  a = .false.
  print *, .not. a
  print *, .not. (.not. .true.)
  x = 5
  print *, .not. (x > 10)
  print *, .not. (x > 0)
end program
