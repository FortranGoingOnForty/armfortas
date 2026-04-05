! Tests abs, max, min intrinsics with proper CSEL codegen.
!
! CHECK: 5
! CHECK: 3
! CHECK: 7
! CHECK: -5
program test_abs_max_min
    implicit none
    integer :: a, b

    a = -5
    b = 3

    print *, abs(a)
    print *, abs(b)
    print *, max(a, b, 7)
    print *, min(a, b, 2)
end program
