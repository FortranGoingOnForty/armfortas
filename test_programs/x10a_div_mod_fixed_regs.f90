! x10a edge case: integer division/modulo fixes rax (quotient) and rdx
! (remainder), and idiv is preceded by cltd/cqto that also write rdx.
! With rax/rdx now in the allocation pool, the fixed-interval check
! must keep every other live value out of rax/rdx across each idiv.
! Many div/mod with their results kept live stresses that check. Seed
! is opaque (command_argument_count()) so nothing folds. OPT_EQ ties
! naive to linear scan.
! FLAGS: --std=f2023
! CHECK: q 36
! CHECK: r 15
! CHECK: combo 51
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program x10a_div_mod_fixed_regs
  implicit none
  integer :: n, q1, q2, q3, r1, r2, r3, qsum, rsum
  n = command_argument_count() + 252   ! 252
  ! Three divisions whose quotients and remainders are all held live
  ! simultaneously (each idiv clobbers rax/rdx while q1/r1/... must
  ! survive in other registers).
  q1 = n / 7      ! 36
  r1 = mod(n, 7)  ! 0
  q2 = n / 11     ! 22
  r2 = mod(n, 11) ! 10
  q3 = n / 13     ! 19
  r3 = mod(n, 13) ! 11
  qsum = q1 + q2 + q3 - 41      ! 36+22+19-41 = 36
  rsum = r1 + r2 + r3 + 0       ! 0+10+11 = 21
  print '(A,1X,I0)', 'q', qsum
  print '(A,1X,I0)', 'r', rsum
  print '(A,1X,I0)', 'combo', qsum + rsum
  print '(A)', 'ok'
end program x10a_div_mod_fixed_regs
