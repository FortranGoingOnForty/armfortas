program integer16_external_call
  implicit none

  interface
    integer(16) function add_ext(x) bind(c, name='add_ext')
      integer(16), value :: x
    end function add_ext
  end interface

  integer(16) :: x, y
  integer :: score

  x = 41_16
  y = add_ext(x)

  if (y == 42_16) then
    score = 1
  else
    score = 0
  end if

  print *, score
end program integer16_external_call
