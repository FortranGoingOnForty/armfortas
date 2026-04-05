! Tests mod intrinsic.
!
! CHECK: 1
! CHECK: 2
program test_mod
    implicit none
    print *, mod(7, 3)
    print *, mod(17, 5)
end program
