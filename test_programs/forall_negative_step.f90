! FORALL with negative step (reverse iteration).
!
! CHECK: 10
! CHECK: 50
program test_forall_neg
    implicit none
    integer, allocatable :: arr(:)
    integer :: i
    allocate(arr(5))
    forall (i = 5:1:-1)
        arr(i) = i * 10
    end forall
    print *, arr(1)
    print *, arr(5)
    deallocate(arr)
end program
