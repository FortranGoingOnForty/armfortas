! CHECK: 6
      PROGRAM FIXEDSUM
      INTEGER I, S
      S = 0
      DO 10 I = 1, 3
         S = S + I
   10 CONTINUE
      PRINT *, S
      END
