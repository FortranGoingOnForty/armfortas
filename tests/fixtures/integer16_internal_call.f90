program integer16_internal_call
  implicit none
  integer(16) :: x, y
  integer :: score

  x = 41_16
  y = add_one(x)

  if (y == 42_16) then
    score = 1
  else
    score = 0
  end if

  print *, score

contains

  integer(16) function add_one(x) result(r)
    integer(16), value :: x

    r = x + 1_16
  end function add_one
end program integer16_internal_call
