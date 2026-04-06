! Whole-array assignment: scalar broadcast and array copy.
!
! CHECK: 42
! CHECK: 42
! CHECK: 42
! CHECK: 42
! CHECK: 42
program test_array_assign
    implicit none
    integer, allocatable :: a(:), b(:)

    allocate(a(5))
    a = 42

    print *, a(1)
    print *, a(3)
    print *, a(5)

    allocate(b(5))
    b = a
    a(1) = 99
    print *, b(1)
    print *, b(5)

    deallocate(a)
    deallocate(b)
end program
