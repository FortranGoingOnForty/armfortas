! Tests bit manipulation intrinsics.
!
! CHECK: 8
! CHECK: 14
! CHECK: 6
program test_bits
    implicit none
    integer :: a, b
    a = 12
    b = 10
    print *, iand(a, b)
    print *, ior(a, b)
    print *, ieor(a, b)
end program
