! Implied-do array constructors: `[(expr, i=start,end[,step])]`.
! Before the fix, store_ac_values_into's ImpliedDo branch
! silently advanced a compile-time counter without emitting
! any stores — the destination array kept whatever stack bytes
! happened to be there.
! Audit BLOCKING-1.
!
! CHECK: 1 4 9 16 25
! CHECK: 10 20 30 40
! CHECK: 11 12 13 21 22 23
program implied_do_array
  integer :: a(5)
  integer :: b(4)
  integer :: c(6)
  integer :: i, j

  ! Basic square sequence
  a = [(i*i, i=1,5)]
  print *, a

  ! Custom step
  b = [(i, i=10,40,10)]
  print *, b

  ! Nested implied-do
  c = [((i*10 + j, j=1,3), i=1,2)]
  print *, c
end program
