program loop_sum
    implicit none
    integer :: i, s
    s = 0
    do i = 1, 10
        s = s + i
    end do
    print *, s
end program
