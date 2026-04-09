! Test SROA: small array decomposed into scalars.
! At O2, the alloca [i32 x 3] should be decomposed into 3 scalar
! allocas, then promoted by mem2reg, then folded by const prop.
program sroa_small_array
  implicit none
  integer :: p(3)
  p(1) = 10
  p(2) = 20
  p(3) = 30
  print *, p(1) + p(2) + p(3)
end program
! CHECK: 60
