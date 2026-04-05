! Internal I/O test.
! Writes an integer to a character variable, then reads it back.
! Exercises: WRITE to character variable, READ from character variable.
!
! CHECK: 42
program test_io_internal
    implicit none
    character(len=20) :: buf
    integer :: x

    write(buf, *) 42
    x = 0
    read(buf, *) x

    print *, x
end program
