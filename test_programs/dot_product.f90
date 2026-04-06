! DOT_PRODUCT: integer and real variants.
!
! CHECK: 32
! CHECK: 3.200000000000000E1
program test_dot_product
    implicit none
    integer, allocatable :: ai(:), bi(:)
    real(8), allocatable :: ar(:), br(:)

    allocate(ai(3), bi(3))
    ai(1) = 1
    ai(2) = 2
    ai(3) = 3
    bi(1) = 4
    bi(2) = 5
    bi(3) = 6
    print *, dot_product(ai, bi)
    deallocate(ai, bi)

    allocate(ar(3), br(3))
    ar(1) = 1.0d0
    ar(2) = 2.0d0
    ar(3) = 3.0d0
    br(1) = 4.0d0
    br(2) = 5.0d0
    br(3) = 6.0d0
    print *, dot_product(ar, br)
    deallocate(ar, br)
end program
