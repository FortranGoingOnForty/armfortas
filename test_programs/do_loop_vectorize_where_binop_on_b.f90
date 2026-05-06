! WHERE then-arm with binop where the main load is on a SECOND array,
! paired with a loop-invariant scalar:
!   where (a > 0) c = K + d
!
! Lifts to vselect(mask, vadd(vbcast(K), vload(d)), vload_c_old).
!
! a(i) = i - 16; d(i) = real(i, 4); c(i) = 0.
! WHERE a > 0 (lanes 17..32): c ← 100 + d (= 117 .. 132).
!   c(1)  = 0.0   (mask=false)
!   c(16) = 0.0   (a(16)=0, NOT > 0)
!   c(17) = 117.0 (mask=true → 100 + d(17) = 100 + 17)
!   c(32) = 132.0 (mask=true → 100 + d(32) = 100 + 32)
!
! CHECK: 0.0000000E0     0.0000000E0     1.1700000E2     1.3200000E2
program test_do_loop_vectorize_where_binop_on_b
  implicit none
  integer :: i
  real(4) :: a(32), c(32), d(32), hundred
  hundred = 100.0
  do i = 1, 32
    a(i) = real(i - 16, 4)
    d(i) = real(i, 4)
    c(i) = 0.0
  end do
  where (a > 0.0)
    c = hundred + d
  end where
  print *, c(1), c(16), c(17), c(32)
end program test_do_loop_vectorize_where_binop_on_b
