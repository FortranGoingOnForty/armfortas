program integer16_ordered_branch
  implicit none
  integer(16) :: x
  integer(16) :: y
  integer(16) :: z
  integer :: score

  x = -4_16
  y = 3_16
  z = -4_16
  score = 0

  if (x < y) score = score + 1
  if (x <= z) score = score + 1
  if (y > z) score = score + 1
  if (y >= x) score = score + 1

  print *, score
end program integer16_ordered_branch
