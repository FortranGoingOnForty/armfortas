! CHECK: empty ok
! CHECK: reassign ok
program test_string_empty
    implicit none
    character(:), allocatable :: s

    ! Empty string assignment — edge case that causes crashes in some compilers.
    s = ''
    ! Assign over empty.
    s = 'something'
    ! Back to empty.
    s = ''
    ! And back again.
    s = 'final'

    print *, 'empty ok'

    ! Reassign from non-empty to shorter.
    s = 'hello world'
    s = 'hi'
    print *, 'reassign ok'
end program
