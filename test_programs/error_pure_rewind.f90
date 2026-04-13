! REWIND in PURE procedure is forbidden (F2018 15.7).
! ERROR_EXPECTED: not allowed in pure
program t
  implicit none
contains
  pure subroutine bad()
    rewind(10)
  end subroutine
end program
