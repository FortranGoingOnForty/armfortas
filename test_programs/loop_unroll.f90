! Loop unrolling correctness test.
! Loops with small constant trip counts must produce identical results
! at -O0, -O2, and -O3. The unroller fires for trip counts ≤ 8.
program loop_unroll
  implicit none
  integer :: a(8), i, s

  ! Trip-4 loop: store const into each element.
  do i = 1, 4
    a(i) = i * 10
  end do
  print *, a(1), a(2), a(3), a(4)   ! 10 20 30 40

  ! Trip-1 loop (trivially unrollable).
  s = 0
  do i = 3, 3
    s = s + i
  end do
  print *, s                          ! 3

  ! Trip-8 loop (boundary of unroll threshold).
  do i = 1, 8
    a(i) = i
  end do
  s = 0
  do i = 1, 8
    s = s + a(i)
  end do
  print *, s                          ! 36

  ! Trip-9 loop (above threshold — not unrolled, must still be correct).
  s = 0
  do i = 1, 9
    s = s + i
  end do
  print *, s                          ! 45

end program loop_unroll
! CHECK: 10 20 30 40
! CHECK: 3
! CHECK: 36
! CHECK: 45
