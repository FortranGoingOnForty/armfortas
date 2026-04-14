! Default implicit typing: names outside I-N are implicit REAL.
! The emitted alloca must be f32 (not i32) so the literal stores
! correctly and the output is a real.
! CHECK: 3.1400001E0
program implicit_real
  x = 3.14
  print *, x
end program
