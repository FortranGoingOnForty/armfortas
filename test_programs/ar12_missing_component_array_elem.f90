program ar12_missing_component_array_elem
  implicit none
  type :: t
    integer :: ok
  end type
  type(t) :: x(2)
  print *, x(1)%bad
end program
! ERROR_EXPECTED: unknown component 'bad'
! ERROR_SPAN: 7:12
