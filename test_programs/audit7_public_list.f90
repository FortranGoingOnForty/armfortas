! Module with PRIVATE default and explicit public :: list.
! Regression test for parsing public :: name1, name2 access-spec list.
! CHECK: 0 0
module public_list_mod
  implicit none
  private
  public :: x, y

  integer :: x = 1
  integer :: y = 2
  integer :: z = 3
end module

program test_public_list
  use public_list_mod
  implicit none
  print *, x, y
end program
