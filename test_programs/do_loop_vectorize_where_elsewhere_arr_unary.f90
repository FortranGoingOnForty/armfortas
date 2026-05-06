! WHERE / ELSEWHERE with unary on else array load:
!   where (a > 0) c = a
!   elsewhere    c = -d
!
! Lifts to vselect(mask, vload(a), vneg(vload(d))) → vstore(c).
!
! a(i) = i - 16; d(i) = real(i, 4); c(i) = 0.
! WHERE a > 0 (lanes 17..32): c ← a (= 1..16).
! ELSEWHERE                    : c ← -d (= -1 .. -16 for lanes 1..16).
!   c(1)  = -1.0  (mask=false → -d(1) = -1)
!   c(16) = -16.0 (a(16)=0, NOT > 0 → -d(16) = -16)
!   c(17) = 1.0   (mask=true → a = 1)
!   c(32) = 16.0  (mask=true → a = 16)
!
! CHECK:    -1.0000000E0    -1.6000000E1     1.0000000E0     1.6000000E1
program test_do_loop_vectorize_where_elsewhere_arr_unary
  implicit none
  integer :: i
  real(4) :: a(32), c(32), d(32)
  do i = 1, 32
    a(i) = real(i - 16, 4)
    d(i) = real(i, 4)
    c(i) = 0.0
  end do
  where (a > 0.0)
    c = a
  elsewhere
    c = -d
  end where
  print *, c(1), c(16), c(17), c(32)
end program test_do_loop_vectorize_where_elsewhere_arr_unary
