! Regression for audit finding M-1: const_fold IntToFloat to f32 must
! round through f32 precision. Before the fix, the optimizer pipeline
! would chain `1.0_4 + real(16777217, 4)` through const_fold without
! ever rounding the integer-to-f32 step, producing 1.6777218E7 instead
! of the correct 1.6777216E7 (the f32 round of 16777217 stays below
! the +1 ULP, so the addition is a no-op at f32 precision).
!
! Both -O0 and -O2 must produce the same output.
!
! CHECK: 1.6777216E7
program test_audit_m1
    implicit none
    real(4) :: r
    r = 1.0_4 + real(16777217, 4)
    print *, r
end program
