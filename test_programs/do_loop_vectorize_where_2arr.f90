! WHERE with an array+array body: `where (a > 0) c = a + b`. Both
! `a` and `b` are arrays loaded per iteration; the matcher must
! recognize the second array load (in the then_block) as a
! vector load and feed both into a VAdd that drives vselect's
! true arm.
!
! a(i) = i - 16 (range -15..16); b(i) = 100; c(i) = i.
! Where a > 0 (lanes 17..32): c ← a + b = (i-16) + 100 = i + 84.
!   c(1) = 1, c(16) = 16, c(17) = 101, c(32) = 116.
!
! CHECK: 1.0000000E0     1.6000000E1     1.0100000E2     1.1600000E2
program test_do_loop_vectorize_where_2arr
  implicit none
  integer :: i
  real(4) :: a(32), b(32), c(32)
  do i = 1, 32
    a(i) = real(i - 16, 4)
    b(i) = 100.0
    c(i) = real(i, 4)
  end do
  where (a > 0.0)
    c = a + b
  end where
  print *, c(1), c(16), c(17), c(32)
end program test_do_loop_vectorize_where_2arr
