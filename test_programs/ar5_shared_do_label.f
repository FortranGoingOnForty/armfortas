! Shared F77 DO termination labels close every open DO using that label.
!
! FLAGS: -Wdeprecated
! WARN_CHECK: shared DO termination label is a deleted feature
! CHECK: shared 102 1332
      PROGRAM AR5SHDO
      INTEGER I, J, K, TWO, THREE
      TWO = 0
      THREE = 0

      DO 10 I = 1, 2
      DO 10 J = 1, 3
      TWO = TWO + I * 10 + J
   10 CONTINUE

      DO 20 I = 1, 2
      DO 20 J = 1, 2
      DO 20 K = 1, 2
      THREE = THREE + I * 100 + J * 10 + K
   20 CONTINUE

      PRINT '(A,1X,I0,1X,I0)', 'shared', TWO, THREE
      END
