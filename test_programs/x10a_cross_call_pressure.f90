! x10a edge case: scalar values live across multiple calls. Each call
! clobbers all caller-saved registers, so values that outlive a call
! must go to a callee-saved register (only 5 GP) or spill — this
! exercises the allocator's caller-saved-across-call routing and the
! callee-save save/restore bracketing. The seed is
! command_argument_count() (0 at runtime, opaque to the optimizer) so
! the live set is not constant-folded away. OPT_EQ ties naive (O0) to
! linear scan (O1+).
! FLAGS: --std=f2023
! CHECK: r 62
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program x10a_cross_call_pressure
  implicit none
  integer :: total
  call run(command_argument_count(), total)
  print '(A,1X,I0)', 'r', total
  print '(A)', 'ok'
contains
  integer function step(x)
    integer, intent(in) :: x
    ! Opaque pass-through (adds 0 at runtime) that forms a real call
    ! boundary the optimizer cannot elide or fold through.
    step = x + command_argument_count()
  end function
  subroutine run(seed, out)
    integer, intent(in) :: seed
    integer, intent(out) :: out
    integer :: a, b, c, d, e, f, g, h
    a = seed + 1
    b = seed + 2
    c = seed + 3
    d = seed + 4
    e = seed + 5
    f = seed + 6
    g = seed + 7
    h = seed + 8
    ! Each step() is a call; the values not threaded through it must
    ! stay live across it. Interleave so the live set straddles calls.
    a = step(a) + h
    b = step(b) + g
    c = step(c) + f
    d = step(d) + e
    out = a + b + c + d + e + f + g + h
  end subroutine
end program x10a_cross_call_pressure
