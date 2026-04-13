! User-defined kind parameter: integer parameter used as kind selector.
! Verifies that user PARAMETER constants resolve correctly in real(my_kind).
! CHECK: user kind x=     3.141592653589793E0
program test_user_kind
  implicit none
  integer, parameter :: my_kind = 8
  real(my_kind) :: x

  x = 3.141592653589793d0
  print *, 'user kind x=', x
end program
