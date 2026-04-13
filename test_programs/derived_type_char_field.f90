! Derived type with character field: assignment and printing.
! CHECK: 30
! CHECK: Alice
program t
  implicit none
  type :: person
    integer :: age
    character(len=20) :: name
  end type
  type(person) :: p
  p%age = 30
  p%name = "Alice"
  print *, p%age
  print *, p%name
end program
