! Small fixed-array fixture for optimization capture.
! At O2, the observable sum should fold to a constant in optimized IR.
program sroa_small_array
  implicit none
  integer :: p(3)
  p(1) = 10
  p(2) = 20
  p(3) = 30
  print *, p(1) + p(2) + p(3)
end program
! CHECK: 60
