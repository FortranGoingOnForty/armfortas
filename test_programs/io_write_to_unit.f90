! Tests WRITE(unit, *) syntax with explicit unit number.
! Exercises: WRITE to file unit, CLOSE, verify content via READ.
!
! CHECK: 100
program test_write_to_unit
    implicit none
    integer :: val

    open(20, file='/tmp/afs_unit_test.dat', status='replace')
    write(20, *) 100
    close(20)

    val = 0
    open(20, file='/tmp/afs_unit_test.dat', status='old')
    read(20, *) val
    close(20)

    print *, val
end program
