! WHERE / ELSEWHERE both arms loading from arrays:
!   where (a > 0) c = a
!   elsewhere    c = b
! lowers to vselect(mask, vload_a, vload_b) → vstore_c.
!
! a(i) = i - 16 (range -15..16); b(i) = 1000.0 + i; c(i) = 0.
! WHERE a > 0 (lanes 17..32): c ← a (= 1..16).
! ELSEWHERE                    : c ← b (= 1001..1016 for lanes 1..16).
!   c(1)  = 1001 (mask=false → b(1))
!   c(16) = 1016 (a(16)=0, NOT > 0 → b(16))
!   c(17) = 1.0  (mask=true → a = 1)
!   c(32) = 16.0 (mask=true → a = 16)
!
! CHECK: 1.0010000E3     1.0160000E3     1.0000000E0     1.6000000E1
program test_do_loop_vectorize_where_elsewhere_arr
  implicit none
  integer :: i
  real(4) :: a(32), b(32), c(32)
  do i = 1, 32
    a(i) = real(i - 16, 4)
    b(i) = real(1000 + i, 4)
    c(i) = 0.0
  end do
  where (a > 0.0)
    c = a
  elsewhere
    c = b
  end where
  print *, c(1), c(16), c(17), c(32)
end program test_do_loop_vectorize_where_elsewhere_arr
