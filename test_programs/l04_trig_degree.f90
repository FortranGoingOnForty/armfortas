! l04: F2023 degree trig (ACOSD/ASIND/ATAND/ATAN2D/COSD/SIND/TAND).
! The families exist for exactness at the special angles — a naive
! x*PI/180 then sin would give ~1e-16, not 0 — so the exact points are
! asserted by equality, and OPT_EQ pins the result identical across
! every level (including -Ofast fast-math, which must not break it).
! FLAGS: --std=f2023
! CHECK: sind180 T
! CHECK: cosd90 T
! CHECK: tand45 T
! CHECK: sind30 T
! CHECK: cosd60 T
! CHECK: atan2d T
! CHECK: acosd T
! CHECK: asind T
! CHECK: atand T
! CHECK: dp T
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program l04_trig_degree
  implicit none
  real :: x
  real(8) :: d

  ! Exact special angles (real(4)).
  print '(A,1X,L1)', 'sind180', sind(180.0) == 0.0
  print '(A,1X,L1)', 'cosd90', cosd(90.0) == 0.0
  print '(A,1X,L1)', 'tand45', tand(45.0) == 1.0

  ! Known midpoints, tolerance-checked.
  x = 30.0
  print '(A,1X,L1)', 'sind30', abs(sind(x) - 0.5) < 1.0e-6
  print '(A,1X,L1)', 'cosd60', abs(cosd(60.0) - 0.5) < 1.0e-6

  ! Inverse family, cardinal points exact.
  print '(A,1X,L1)', 'atan2d', atan2d(0.0, -1.0) == 180.0
  print '(A,1X,L1)', 'acosd', acosd(-1.0) == 180.0
  print '(A,1X,L1)', 'asind', asind(1.0) == 90.0
  print '(A,1X,L1)', 'atand', atand(1.0) == 45.0

  ! real(8) path uses the f64 runtime symbol directly.
  d = 180.0d0
  print '(A,1X,L1)', 'dp', sind(d) == 0.0d0 .and. tand(45.0d0) == 1.0d0

  print '(A)', 'ok'
end program l04_trig_degree
