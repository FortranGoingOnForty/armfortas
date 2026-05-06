! WHERE with a binop in the body using a loop-invariant scalar
! (`b = a + K`, `b = a * scale`, etc.). The scalar is broadcast in
! the preheader and the binop is lifted to a vector binop on the
! vload result, which feeds the vselect's true arm.
!
! Test 1: `where (a > 0); b = a + 100; end where` — adds a constant.
! Test 2: `where (a > 0); c = a * 10; end where` — scales by a constant.
! a = [-15..16]; b/c initial = [1..32].
!
! Lanes where a > 0 (i = 17..32) get b ← a+100, c ← a*10:
!   b(1)=1, b(16)=16, b(17)=101, b(32)=116
!   c(1)=1, c(16)=16, c(17)=10,  c(32)=160
!
! CHECK: 1.0000000E0     1.6000000E1     1.0100000E2     1.1600000E2
! CHECK: 1.0000000E0     1.6000000E1     1.0000000E1     1.6000000E2
program test_do_loop_vectorize_where_binop
  implicit none
  integer :: i
  real(4) :: a(32), b(32), c(32)
  do i = 1, 32
    a(i) = real(i - 16, 4)
    b(i) = real(i, 4)
    c(i) = real(i, 4)
  end do
  where (a > 0.0)
    b = a + 100.0
  end where
  where (a > 0.0)
    c = a * 10.0
  end where
  print *, b(1), b(16), b(17), b(32)
  print *, c(1), c(16), c(17), c(32)
end program test_do_loop_vectorize_where_binop
