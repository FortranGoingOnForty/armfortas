! Direct access I/O roundtrip.
! Writes records out of order, reads them back in order.
! Exercises: OPEN with access='direct', recl=, WRITE/READ with rec=.
!
! CHECK: 30
! CHECK: 10
! CHECK: 20
program test_io_direct_access
    implicit none
    integer :: a, b, c

    open(10, file='/tmp/afs_direct.dat', access='direct', recl=4, &
         form='unformatted', status='replace')

    ! Write records out of order: rec 3, rec 1, rec 2.
    write(10, rec=3) 30
    write(10, rec=1) 10
    write(10, rec=2) 20
    close(10)

    a = 0; b = 0; c = 0
    open(10, file='/tmp/afs_direct.dat', access='direct', recl=4, &
         form='unformatted', status='old')
    read(10, rec=3) a
    read(10, rec=1) b
    read(10, rec=2) c
    close(10)

    print *, a
    print *, b
    print *, c
end program
