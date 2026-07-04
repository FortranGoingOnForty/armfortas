! l10 recorded decision: internal READ from a whole character array
! produced silent garbage (len-0 buffer view); until the read path
! grows record-per-element semantics it is rejected loudly. Element
! units still work.
! ERROR_EXPECTED: internal READ from a whole character array is not implemented
program l10_internal_read_array_reject
  implicit none
  character(len=8) :: rec(3)
  integer :: a, b, c
  rec = ' '
  read(rec, '(i8)') a, b, c
  print *, a, b, c
end program l10_internal_read_array_reject
