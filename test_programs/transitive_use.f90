! Transitive USE: program uses C which uses B which uses A.
! All three module variables must be accessible from the program.
! CHECK: 10 20 30
module a
  implicit none
  integer :: va = 10
end module
module b
  use a
  implicit none
  integer :: vb = 20
end module
module c
  use b
  implicit none
  integer :: vc = 30
end module
program p
  use c
  implicit none
  print *, va, vb, vc
end program
