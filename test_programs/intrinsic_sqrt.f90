! Tests sqrt intrinsic via ARM64 FSQRT instruction.
!
! CHECK: 3.0
program test_sqrt
    implicit none
    real :: x
    x = 9.0
    print *, sqrt(x)
end program
