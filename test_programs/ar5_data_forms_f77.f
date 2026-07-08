! Fixed-form DATA repeat counts and implied-do object lists.
!
! CHECK: fixed 6 6 6
! CHECK: fixed_chars [q ][rs]
      PROGRAM AR5DATAF
      INTEGER A(3), I
      CHARACTER*2 C(2)
      DATA (A(I), I=1,3) / 3*6 /
      DATA C / 'q', 'rs' /
      PRINT '(A,3(I0,1X))', 'fixed ', A
      PRINT '(5A)', 'fixed_chars [', C(1), '][', C(2), ']'
      END
