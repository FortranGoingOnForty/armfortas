! Derived type with character component, defined in a module.
! Exercises module procedure calls with derived type arguments.
! CHECK: Name:Fortran
! CHECK: Age: 30
module dtype_mod
  implicit none
  type :: person_t
    character(len=20) :: name
    integer :: age
  end type
contains
  subroutine print_person(p)
    type(person_t), intent(in) :: p
    print *, 'Name:', p%name
    print *, 'Age:', p%age
  end subroutine
end module

program test_dtype
  use dtype_mod
  implicit none
  type(person_t) :: p

  p%name = 'Fortran'
  p%age = 30
  call print_person(p)
end program
