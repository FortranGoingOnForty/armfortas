! Array intrinsics on REAL arrays: SUM, MAXVAL, MINVAL with type dispatch.
!
! CHECK: 6.0
! CHECK: 3.0
! CHECK: 1.0
program test_real_array
    implicit none
    real(8), allocatable :: a(:)
    allocate(a(3))
    a(1) = 1.0d0
    a(2) = 2.0d0
    a(3) = 3.0d0
    print *, sum(a)
    print *, maxval(a)
    print *, minval(a)
    deallocate(a)
end program
