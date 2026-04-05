! BACKSPACE test.
! Writes two records, backspaces one, re-reads the second record.
! Exercises: BACKSPACE statement, record-level positioning.
!
! CHECK: 20
program test_io_backspace
    implicit none
    integer :: x

    open(10, file='/tmp/afs_backspace.dat', status='replace', action='readwrite')
    write(10, *) 10
    write(10, *) 20
    ! Position is after record 2. Backspace to before record 2.
    backspace(10)
    x = 0
    read(10, *) x
    close(10)

    print *, x
end program
