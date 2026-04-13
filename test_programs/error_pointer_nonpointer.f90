! Pointer assignment LHS must have pointer attribute.
! ERROR_EXPECTED: must have pointer attribute
program t
  implicit none
  integer, target :: tgt = 42
  integer :: not_a_ptr
  not_a_ptr => tgt
end program
