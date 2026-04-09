! Audit #6 probe — character(len=:), allocatable.
!
! Deferred-length allocatable strings are gfortran's #1 ARM64
! failure mode and a hard fortsh requirement. This program
! exercises the basic allocate / assign / print / deallocate /
! re-allocate cycle.
!
! CHECK: Hello
! CHECK: World
program audit6_char_allocatable
  character(len=:), allocatable :: s
  allocate(character(len=10) :: s)
  s = "Hello"
  print *, s
  deallocate(s)
  allocate(character(len=5) :: s)
  s = "World"
  print *, s
end program
