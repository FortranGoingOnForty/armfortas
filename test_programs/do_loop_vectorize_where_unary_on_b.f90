! WHERE then-arm with unary on a SECOND array load:
!   where (a > 0) c = -d
!
! a(i) = i - 16; d(i) = real(i, 4); c(i) = 0.
! WHERE a > 0 (lanes 17..32): c ← -d (= -17 .. -32).
! ELSE (default lowering): c stays at 0.
!   c(1)  = 0.0   (mask=false, stays 0)
!   c(16) = 0.0   (a(16)=0, NOT > 0)
!   c(17) = -17.0 (mask=true → -d(17))
!   c(32) = -32.0 (mask=true → -d(32))
!
! CHECK: 0.0000000E0     0.0000000E0    -1.7000000E1    -3.2000000E1
program test_do_loop_vectorize_where_unary_on_b
  implicit none
  integer :: i
  real(4) :: a(32), c(32), d(32)
  do i = 1, 32
    a(i) = real(i - 16, 4)
    d(i) = real(i, 4)
    c(i) = 0.0
  end do
  where (a > 0.0)
    c = -d
  end where
  print *, c(1), c(16), c(17), c(32)
end program test_do_loop_vectorize_where_unary_on_b
