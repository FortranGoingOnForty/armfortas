! Regression: named ELSE and ELSE IF clauses must parse. A named IF
! construct may repeat the construct name on its ELSE / ELSE IF ... THEN
! statements (`else name`, `else if (c) then name`). armfortas's parser
! consumed the name on the opening `name:` and on `end if name`, but not
! on ELSE/ELSE IF, so the trailing name token was mis-parsed as a
! statement ("subroutine calls require CALL"). Surfaced building fpm: its
! vendored M_CLI2 (decodebase) uses `ALL: if (...) ... else ALL ...`. x12.
!
! CHECK: classify(-3) = 0
! CHECK: classify(0) = 1
! CHECK: classify(7) = 2
program p
  implicit none
  write(*, '(a,i0)') 'classify(-3) = ', classify(-3)
  write(*, '(a,i0)') 'classify(0) = ', classify(0)
  write(*, '(a,i0)') 'classify(7) = ', classify(7)
contains
  integer function classify(n) result(r)
    integer, intent(in) :: n
    chk: if (n < 0) then
      r = 0
    else if (n == 0) then chk
      r = 1
    else chk
      r = 2
    end if chk
  end function classify
end program p
