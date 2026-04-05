! Tests trigonometric intrinsics via libm.
! sin(0) = 0, cos(0) = 1.
!
! CHECK: 0.0
! CHECK: 1.0
program test_trig
    implicit none
    double precision :: x
    x = 0.0d0
    print *, sin(x)
    print *, cos(x)
end program
