! Tests list-directed WRITE of all intrinsic types to stdout.
! Exercises: integer, real, logical, character, mixed types in one statement.
!
! CHECK: 42
! CHECK: 3.14
! CHECK: T
! CHECK: hello
! CHECK: x = 10
program test_io_write_types
    implicit none
    integer :: i
    real :: r
    logical :: flag

    i = 42
    r = 3.14
    flag = .true.

    print *, i
    print *, r
    print *, flag
    print *, 'hello'
    print *, 'x =', 10
end program
