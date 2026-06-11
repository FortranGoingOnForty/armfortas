! l02: .NIL. is only legal as a conditional-argument arm in a CALL.
! FLAGS: --std=f2023
! ERROR_EXPECTED: .NIL. is only valid as an arm of a conditional actual
program l02_nil_misuse
  implicit none
  integer :: x
  x = (.true. ? 1 : .nil.)
  print *, x
end program l02_nil_misuse
