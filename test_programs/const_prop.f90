! Constant propagation: a constant IF condition lets const-prop fold
! the conditional branch into an unconditional one and prune the dead
! arm. Both arms produce different output, so an incorrect fold would
! be obvious.
!
! Note: this test originally used `logical, parameter :: pick = .true.`
! which exposes a separate lowering bug — `parameter` constants are
! emitted as alloca + load with no intervening store, so the load
! reads uninitialized memory. At -O0 the load returned whatever bytes
! were on the stack (often non-zero, picking the "alive" arm by
! accident); after mem2reg the load is replaced with `Undef` and the
! cond_br takes whichever path codegen lowers undef as. The fix is
! in the parameter lowering, tracked in `.docs/noted_issues.md`. For
! now we use a normal logical with an explicit assignment so this
! test exercises mem2reg + const_prop without depending on the bug.
!
! CHECK: alive
program test_const_prop
    implicit none
    logical :: pick
    pick = .true.

    if (pick) then
        print *, "alive"
    else
        print *, "dead"
    end if
end program
