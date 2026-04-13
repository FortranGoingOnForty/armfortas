! READ in PURE procedure is forbidden (F2018 15.7).
! ERROR_EXPECTED: not allowed in pure
program t
  implicit none
contains
  pure integer function bad()
    integer :: x
    read *, x
    bad = x
  end function
end program
