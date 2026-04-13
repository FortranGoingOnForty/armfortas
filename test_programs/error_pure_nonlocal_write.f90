! PURE procedure cannot write host-associated variable (F2018 15.7).
! ERROR_EXPECTED: host or use association
program t
  implicit none
  integer :: host_var = 0
contains
  pure subroutine bad()
    host_var = 42
  end subroutine
end program
