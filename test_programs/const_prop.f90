! Constant propagation: a constant IF condition lets const-prop fold
! the conditional branch into an unconditional one and prune the dead
! arm. Both arms produce different output, so an incorrect fold would
! be obvious.
!
! CHECK: alive
program test_const_prop
    implicit none
    logical, parameter :: pick = .true.

    if (pick) then
        print *, "alive"
    else
        print *, "dead"
    end if
end program
