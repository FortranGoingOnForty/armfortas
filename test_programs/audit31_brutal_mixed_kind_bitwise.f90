! audit31 Finding 14: `iand(val_c_long, not(int(z'400', c_int)))`
! tripped the IR verifier with "bitwise op: operand width mismatch
! i64 vs i32". F2018 §16.9.104 doesn't require iand/ior/ieor to
! receive same-kind operands; unify operand widths before emitting
! the bit_and. Task #495.
! CHECK: 768
program audit31_mixed_kind_bitwise
  use iso_c_binding
  implicit none
  integer(c_long) :: val = int(z'700', c_long)
  integer(c_int) :: mask
  mask = int(z'400', c_int)
  print *, iand(val, int(not(mask), c_long))
end program
