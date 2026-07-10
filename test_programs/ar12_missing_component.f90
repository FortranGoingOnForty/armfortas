program ar12_missing_component
  implicit none
  type :: t
    integer :: ok
  end type
  type(t) :: x
  print *, x%nope
end program
! ERROR_EXPECTED: unknown component 'nope'
! ERROR_SPAN: 7:12
