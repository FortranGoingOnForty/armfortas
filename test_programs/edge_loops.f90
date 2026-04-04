! CHECK: 0
! CHECK: 22
program test_edge_loops
    implicit none
    integer :: i, s

    ! Zero iterations: do i = 1, 0
    s = 0
    do i = 1, 0
        s = s + 1
    end do
    print *, s

    ! Step loop: do i = 1, 10, 3 → 1, 4, 7, 10
    s = 0
    do i = 1, 10, 3
        s = s + i
    end do
    print *, s
end program
