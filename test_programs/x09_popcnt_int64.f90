! popcnt/poppar on integer(int64) values and variables. The x86 isel
! selected the i64->i64 unsigned extend feeding afs_popcount as a
! 32-bit move, so the argument's spill slot kept stale upper bytes and
! popcount counted garbage (found by cli_driver's bit-manipulation
! test when x09 opened the suite on ELF hosts).
! CHECK: 2 0
! CHECK: 3 1
! CHECK: 32 0
! CHECK: 64 0
program x09_popcnt_int64
  use iso_fortran_env, only: int64
  implicit none
  integer(int64) :: v
  v = 5_int64
  print '(I0,1X,I0)', popcnt(v), poppar(v)
  v = 7_int64
  print '(I0,1X,I0)', popcnt(v), poppar(v)
  v = 6148914691236517205_int64 ! 0x5555555555555555: alternating bits
  print '(I0,1X,I0)', popcnt(v), poppar(v)
  v = -1_int64
  print '(I0,1X,I0)', popcnt(v), poppar(v)
end program
