program integer16_internal_stack_call
  implicit none
  integer(16) :: a, b, c, d, e, r
  integer :: score

  a = 1_16
  b = 2_16
  c = 3_16
  d = 4_16
  e = 5_16

  r = add5(a, b, c, d, e)

  if (r == 15_16) then
    score = 1
  else
    score = 0
  end if

  print *, score

contains

  integer(16) function add5(a, b, c, d, e) result(r)
    integer(16), value :: a, b, c, d, e

    r = a + b + c + d + e
  end function add5
end program integer16_internal_stack_call
