! ERROR STOP in PURE procedure is forbidden (F2018 15.7).
! ERROR_EXPECTED: not allowed in pure
program t
  implicit none
contains
  pure subroutine bad()
    error stop "abort"
  end subroutine
end program
