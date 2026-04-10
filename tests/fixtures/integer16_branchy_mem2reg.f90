program integer16_branchy_mem2reg
  integer(16) :: x

  x = 7_16
  if (command_argument_count() .lt. 0) then
    x = 11_16
  else
    x = 7_16
  end if

  if (x .eq. 7_16) then
    print *, 1
  else
    print *, 0
  end if
end program integer16_branchy_mem2reg
