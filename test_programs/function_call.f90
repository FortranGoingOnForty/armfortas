! CHECK: 25
! PHASE_TRIANGULATE: ir|asm|obj|repro
program test_func
    implicit none
    integer :: x, y
    x = 5
    y = square(x)
    print *, y
contains
    function square(n) result(r)
        integer, intent(in) :: n
        integer :: r
        r = n * n
    end function
end program
