program bounds_check_oob
  implicit none
  integer :: a(4)

  a = [1, 2, 3, 4]
  print *, a(5)
end program bounds_check_oob
