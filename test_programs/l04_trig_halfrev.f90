! l04: F2023 half-revolution trig (ACOSPI/ASINPI/ATANPI/ATAN2PI/
! COSPI/SINPI/TANPI). Argument is in half-revolutions (x half-revs =
! x*pi radians); exactness at the quarter/half points is the contract,
! held identical across all opt levels by OPT_EQ.
! FLAGS: --std=f2023
! CHECK: sinpi1 T
! CHECK: cospi05 T
! CHECK: sinpi05 T
! CHECK: tanpi025 T
! CHECK: atan2pi T
! CHECK: acospi T
! CHECK: asinpi T
! CHECK: dp T
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program l04_trig_halfrev
  implicit none
  real(8) :: d

  print '(A,1X,L1)', 'sinpi1', sinpi(1.0) == 0.0
  print '(A,1X,L1)', 'cospi05', cospi(0.5) == 0.0
  print '(A,1X,L1)', 'sinpi05', sinpi(0.5) == 1.0
  print '(A,1X,L1)', 'tanpi025', tanpi(0.25) == 1.0
  print '(A,1X,L1)', 'atan2pi', atan2pi(0.0, -1.0) == 1.0
  print '(A,1X,L1)', 'acospi', acospi(-1.0) == 1.0
  print '(A,1X,L1)', 'asinpi', asinpi(1.0) == 0.5

  d = 0.5d0
  print '(A,1X,L1)', 'dp', cospi(d) == 0.0d0 .and. sinpi(1.0d0) == 0.0d0

  print '(A)', 'ok'
end program l04_trig_halfrev
