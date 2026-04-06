! Nested derived types with chained component access (x%inner%field).
!
! CHECK: 100
! CHECK: 55
! CHECK: 1.23
program test_nested
    implicit none
    type :: inner_t
        integer :: a
        real :: b
    end type
    type :: outer_t
        type(inner_t) :: child
        integer :: tag
    end type
    type(outer_t) :: obj
    obj%tag = 100
    obj%child%a = 55
    obj%child%b = 1.23
    print *, obj%tag
    print *, obj%child%a
    print *, obj%child%b
end program
