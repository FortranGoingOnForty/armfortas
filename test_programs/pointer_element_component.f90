! Pointer association to array elements and derived-type components.
! Previously PointerAssignment only handled Expr::Name RHS.
program pointer_element_component
  implicit none
  type :: pt
    integer :: x, y
  end type
  integer, target :: arr(5)
  integer, pointer :: pe
  type(pt), target :: obj
  integer, pointer :: px
  integer :: i

  do i = 1, 5
    arr(i) = i * 10
  end do
  pe => arr(3)
  print *, pe
  pe = 999
  print *, arr(3)

  obj%x = 7; obj%y = 9
  px => obj%x
  print *, px
  px = 42
  print *, obj%x
end program
! CHECK: 30
! CHECK: 999
! CHECK: 7
! CHECK: 42
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
