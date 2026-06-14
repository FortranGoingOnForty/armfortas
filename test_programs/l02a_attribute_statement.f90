! l02a item 6: standalone ALLOCATABLE/POINTER/TARGET attribute statements.
! The parser folds each into the named entity's type declaration (splitting
! a multi-entity declaration so only that entity gets the attribute), so the
! ordinary type-declaration lowering handles storage.
! FLAGS: --std=f2023
program l02a_attribute_statement
  implicit none
  integer :: y
  allocatable :: y
  integer :: a, b
  allocatable :: a        ! a becomes allocatable; b stays a plain scalar
  integer :: t
  integer :: q
  target :: t
  pointer :: q

  allocate(y)
  y = 7
  print '(I0)', y
  ! CHECK: 7
  deallocate(y)

  b = 5
  allocate(a)
  a = 9
  print '(2(I0,1X))', a, b
  ! CHECK: 9 5
  deallocate(a)

  t = 11
  q => t
  print '(I0)', q
  ! CHECK: 11
  ! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|exit
end program l02a_attribute_statement
