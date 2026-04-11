program integer16_external_stack_call
  implicit none

  interface
    integer(16) function add5_ext(a, b, c, d, e) bind(c, name='add5_ext')
      integer(16), value :: a, b, c, d, e
    end function add5_ext
  end interface

  integer(16) :: a, b, c, d, e, r
  integer :: score

  a = 1_16
  b = 2_16
  c = 3_16
  d = 4_16
  e = 5_16

  r = add5_ext(a, b, c, d, e)

  if (r == 15_16) then
    score = 1
  else
    score = 0
  end if

  print *, score
end program integer16_external_stack_call
