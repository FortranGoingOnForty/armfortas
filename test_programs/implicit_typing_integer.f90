! Default implicit typing (no IMPLICIT NONE): a name starting with
! I-N is an implicit INTEGER. The assignment must materialize a real
! store, not silently drop into const_int 0.
! CHECK: 42
program implicit_int
  i = 42
  print *, i
end program
