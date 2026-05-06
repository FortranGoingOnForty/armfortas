! WHERE with a unary operation in the body (`b = -a` and `b = abs(a)`).
! The matcher recognizes the unary in the then_block and lifts it to
! a vector unary on the vload result, which then feeds the vselect's
! true arm.
!
! Test 1: `where (a > 0); b = -a; end where` over a = [-15..16],
! b = [1..32]. Lanes where a > 0 (i=17..32) get b ← -a:
!   b(1)=1, b(16)=16, b(17)=-1, b(32)=-16.
! Test 2: `where (a < 0); c = abs(a); end where` over the same a,
! c = [1..32]. Lanes where a < 0 (i=1..15) get c ← |a|:
!   c(1)=15, c(15)=1, c(16)=16, c(32)=32.
!
! CHECK: 1.0000000E0     1.6000000E1    -1.0000000E0    -1.6000000E1
! CHECK: 1.5000000E1     1.0000000E0     1.6000000E1     3.2000000E1
program test_do_loop_vectorize_where_unary
  implicit none
  integer :: i
  real(4) :: a(32), b(32), c(32)
  do i = 1, 32
    a(i) = real(i - 16, 4)
    b(i) = real(i, 4)
    c(i) = real(i, 4)
  end do
  where (a > 0.0)
    b = -a
  end where
  where (a < 0.0)
    c = abs(a)
  end where
  print *, b(1), b(16), b(17), b(32)
  print *, c(1), c(15), c(16), c(32)
end program test_do_loop_vectorize_where_unary
