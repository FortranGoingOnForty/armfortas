program ar4_complex_component_io
  implicit none

  type :: item
    real :: arr(2)
  end type

  type(item) :: x
  complex :: c

  c = (3.5, -1.0)
  x%arr = [7.5, 8.5]

  print "(F4.1,F5.1)", c
  ! CHECK: 3.5 -1.0

  print "(F4.1,1X,F4.1)", x%arr
  ! CHECK: 7.5  8.5

  print *, c
  ! CHECK: (   3.5000000E0,  -1.0000000E0)

  print *, x%arr
  ! CHECK: 7.5000000E0     8.5000000E0
end program
