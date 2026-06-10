! l01: DO CONCURRENT REDUCE locality spec (F2023 11.1.7.2 R1131).
! Frontend and runtime verified during the l00 inventory; this is the
! first checked-in fixture. Sum and max reductions over 1..10.
! FLAGS: --std=f2023
! CHECK: 55
! CHECK: 10
program l01_do_concurrent_reduce
  implicit none
  integer :: i, s, m
  s = 0
  m = -huge(m)
  do concurrent (i = 1:10) reduce(+:s) reduce(max:m)
    s = s + i
    m = max(m, i)
  end do
  print *, s
  print *, m
end program l01_do_concurrent_reduce
