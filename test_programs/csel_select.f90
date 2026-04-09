! Test Select lowering for simple conditional assignments.
! At all opt levels, simple if/else assignments to the same scalar
! should lower to Select IR → CSEL on ARM64.
program csel_select
  implicit none
  call test_select(5, 3)
  call test_select(2, 7)
end program csel_select

subroutine test_select(a, b)
  implicit none
  integer, intent(in) :: a, b
  integer :: r

  ! Simple diamond: if (a > b) r = a; else r = b
  ! This is effectively MAX(a, b).
  if (a > b) then
    r = a
  else
    r = b
  end if
  print *, r

  ! Another diamond: conditional value selection.
  if (a + b > 6) then
    r = 100
  else
    r = 200
  end if
  print *, r
end subroutine test_select
! CHECK: 5
! CHECK: 100
! CHECK: 7
! CHECK: 100
