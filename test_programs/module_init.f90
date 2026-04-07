! Module-level variable with initializer. The previous lowerer
! emitted module variables as zero-initialized globals with an
! invalid name (`m::counter`), and never wired use-import name
! resolution, so prints showed 0 instead of 10 and the assembler
! choked on the `::` in the symbol name.
! Audit MAJOR-3.
!
! CHECK: 10
! CHECK: 15
! CHECK: 100
module mod_init
  integer :: counter = 10
  integer :: threshold = 100
end module mod_init

program p
  use mod_init
  print *, counter
  counter = counter + 5
  print *, counter
  print *, threshold
end program
