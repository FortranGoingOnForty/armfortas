! Regression: an internal procedure that accesses a host variable must
! work even when called from a SIBLING internal procedure (not just
! directly from the host). armfortas's host-association closure ABI passes
! a contained proc's host-referenced variables as hidden by-reference args;
! the per-proc ref list was computed from direct references plus NESTED
! children only, so an intermediate sibling that merely CALLS another
! sibling (without referencing the host var itself) carried no hidden param
! for it and forwarded a garbage pointer -> SIGSEGV. Surfaced building
! fortsh: its unit-test harnesses are `program ... contains` with `test_x`
! subroutines calling sibling `assert_*` helpers that bump host pass/fail
! counters (test_suggestions, test_syntax_highlight both crashed on the
! first assert). The fix forwards a callee peer's host-refs through the
! caller. x12 campaign.
!
! CHECK: inner saw total=1 a=8 b=8
! CHECK: total=2 passed=2
program x12_ipc
  implicit none
  integer :: total = 0, passed = 0

  ! Direct host->inner call (one level) AND host->outer->inner (two levels,
  ! the bug): inner reads/writes host total/passed though outer doesn't
  ! reference them at all.
  call inner(8, 8)
  call outer()
  write(*, '(a,i0,a,i0)') 'total=', total, ' passed=', passed
contains
  subroutine inner(a, b)
    integer, intent(in) :: a, b
    total = total + 1
    if (a == b) then
      passed = passed + 1
      write(*, '(a,i0,a,i0,a,i0)') 'inner saw total=', total, ' a=', a, ' b=', b
    end if
  end subroutine

  subroutine outer()
    ! outer references no host var; it only forwards to its sibling inner.
    call inner(8, 8)
  end subroutine
end program
