! Regression: deep recursion must not overflow the stack. armfortas used
! a naive spill-everything register allocator at -O0 that gave every SSA
! value its own stack slot, inflating frames ~6x; fortsh's executor
! SIGSEGV'd recursing past depth ~62. The fix uses the linear-scan
! allocator at every opt level (naive only behind ARMFORTAS_USE_NAIVE_REGALLOC),
! matching gfortran's real register allocation at -O0.
!
! rec() carries a long straight-line chain of short-lived temporaries
! that net to zero, so a good allocator reuses a couple of registers and
! keeps the frame tiny, while spill-everything reserves a slot per step.
! At depth 12000 the naive frame overflows 8 MB; the linear frame does
! not. Result is sum(1..N) = N*(N+1)/2; N=12000 -> 72006000. x12.
!
! CHECK: 72006000
module m
contains
  recursive function rec(n) result(s)
    integer, intent(in) :: n
    integer :: s, t
    if (n <= 0) then
      s = 0
      return
    end if
    t = n
    t = t + 1
    t = t - 1
    t = t + 2
    t = t - 2
    t = t + 3
    t = t - 3
    t = t + 4
    t = t - 4
    t = t + 5
    t = t - 5
    t = t + 6
    t = t - 6
    t = t + 7
    t = t - 7
    t = t + 8
    t = t - 8
    t = t + 9
    t = t - 9
    t = t + 10
    t = t - 10
    t = t + 11
    t = t - 11
    t = t + 12
    t = t - 12
    t = t + 13
    t = t - 13
    t = t + 14
    t = t - 14
    t = t + 15
    t = t - 15
    t = t + 16
    t = t - 16
    t = t + 17
    t = t - 17
    t = t + 18
    t = t - 18
    t = t + 19
    t = t - 19
    t = t + 20
    t = t - 20
    t = t + 21
    t = t - 21
    t = t + 22
    t = t - 22
    t = t + 23
    t = t - 23
    t = t + 24
    t = t - 24
    t = t + 25
    t = t - 25
    t = t + 26
    t = t - 26
    t = t + 27
    t = t - 27
    t = t + 28
    t = t - 28
    t = t + 29
    t = t - 29
    t = t + 30
    t = t - 30
    t = t + 31
    t = t - 31
    t = t + 32
    t = t - 32
    t = t + 33
    t = t - 33
    t = t + 34
    t = t - 34
    t = t + 35
    t = t - 35
    t = t + 36
    t = t - 36
    t = t + 37
    t = t - 37
    t = t + 38
    t = t - 38
    t = t + 39
    t = t - 39
    t = t + 40
    t = t - 40
    t = t + 41
    t = t - 41
    t = t + 42
    t = t - 42
    t = t + 43
    t = t - 43
    t = t + 44
    t = t - 44
    t = t + 45
    t = t - 45
    t = t + 46
    t = t - 46
    t = t + 47
    t = t - 47
    t = t + 48
    t = t - 48
    t = t + 49
    t = t - 49
    t = t + 50
    t = t - 50
    t = t + 51
    t = t - 51
    t = t + 52
    t = t - 52
    t = t + 53
    t = t - 53
    t = t + 54
    t = t - 54
    t = t + 55
    t = t - 55
    t = t + 56
    t = t - 56
    t = t + 57
    t = t - 57
    t = t + 58
    t = t - 58
    t = t + 59
    t = t - 59
    t = t + 60
    t = t - 60
    s = t + rec(n - 1)
  end function
end module
program p
  use m
  print *, rec(12000)
end program
