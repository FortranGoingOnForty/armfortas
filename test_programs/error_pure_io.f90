! I/O in PURE procedure is forbidden (F2018 15.7).
! ERROR_EXPECTED: not allowed in pure
program t
  implicit none
  print *, pure_func()
contains
  pure integer function pure_func()
    print *, "side effect"
    pure_func = 1
  end function
end program
