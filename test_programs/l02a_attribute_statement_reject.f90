! l02a item 6 boundary: a standalone ALLOCATABLE/POINTER/TARGET statement
! carries only entity names — an array-spec on it (`allocatable :: a(:)`)
! has nowhere to fold its shape, so it is rejected loudly. Declare the
! shape on the type declaration instead (`integer, allocatable :: a(:)`).
! FLAGS: --std=f2023
! ERROR_EXPECTED: array-spec in a standalone ALLOCATABLE/POINTER/TARGET
program l02a_attribute_statement_reject
  implicit none
  integer :: a
  allocatable :: a(:)
  allocate(a(2))
  print *, a
end program l02a_attribute_statement_reject
