! CHECK: 2
program test_dse_same_element_twice
    implicit none
    integer :: arr(3)

    arr(1) = 1
    arr(1) = 2

    print *, arr(1)
end program
