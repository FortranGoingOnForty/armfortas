! CHECK: 50
program test_licm_noalias_dummy_load
    implicit none
    integer :: arr(10), total

    call kernel(5, arr, 10, total)
    print *, total

contains

    recursive subroutine kernel(a, b, n, sum_out)
        integer, intent(in) :: a
        integer, intent(out) :: b(10)
        integer, intent(in) :: n
        integer, intent(out) :: sum_out
        integer :: i

        if (n < 0) then
            call kernel(a, b, n, sum_out)
            return
        end if

        sum_out = 0
        do i = 1, 10
            b(i) = i
            sum_out = sum_out + a
        end do
    end subroutine

end program
