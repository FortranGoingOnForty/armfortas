! SAVE attribute in PURE procedure is forbidden (F2018 15.7 C1597).
! ERROR_EXPECTED: not allowed in pure
program t
  implicit none
contains
  pure integer function bad()
    integer, save :: counter = 0
    counter = counter + 1
    bad = counter
  end function
end program
