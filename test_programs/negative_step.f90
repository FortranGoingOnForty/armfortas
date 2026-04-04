! CHECK: 5
! CHECK: 4
! CHECK: 3
! CHECK: 2
! CHECK: 1
program test_negative_step
    implicit none
    integer :: i
    do i = 5, 1, -1
        print *, i
    end do
end program
