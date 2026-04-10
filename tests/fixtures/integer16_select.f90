program integer16_select
  implicit none
  integer(16) :: a, b, c
  integer :: score

  a = 42_16
  b = 17_16

  if (a > b) then
    c = a
  else
    c = b
  end if

  if (c == 42_16) then
    score = 1
  else
    score = 0
  end if

  print *, score
end program integer16_select
