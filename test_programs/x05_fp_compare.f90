! Sprint x05 curated program: FP arithmetic with comparisons driving
! control flow (exercises the ucomi condition table end to end).
! CHECK: 4
! CHECK: 0.316406
program x05_fp_compare
  implicit none
  real(8) :: x, step
  integer :: n
  x = 1.0d0
  step = 0.75d0
  n = 0
  do while (x > 0.4d0)
    x = x * step
    n = n + 1
  end do
  if (x <= 0.4d0 .and. x >= 0.0d0) then
    print *, n
  else
    print *, -1
  end if
  write (*, '(F8.6)') x
end program x05_fp_compare
