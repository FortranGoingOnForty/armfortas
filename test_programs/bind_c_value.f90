! Tests BIND(C) with VALUE attribute.
! Defines a function that receives args by value (C convention)
! and calls it from the main program via an interface block.
!
! CHECK: 30
program test_bind_c_value
    implicit none

    interface
        integer function add_values(a, b) bind(c, name='add_values')
            integer, value :: a, b
        end function
    end interface

    print *, add_values(10, 20)
end program

integer function add_values(a, b) bind(c, name='add_values')
    integer, value :: a, b
    add_values = a + b
end function
