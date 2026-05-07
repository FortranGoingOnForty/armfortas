! WHERE/ELSEWHERE composing with binop in then-arm:
!   where (a > 0) c = a * 2
!   elsewhere    c = -1
! lowers to vselect(mask, vmul(va, vbcast(2)), vbcast(-1)) → vstore.
!
! a(i) = i - 16; c starts as 0.
! WHERE a > 0 (lanes 17..32): c ← a*2 = 2..32.
! ELSEWHERE                    : c ← -1.
!   c(1)  = -1   (a(1)=-15, mask=false)
!   c(16) = -1   (a(16)=0, NOT > 0)
!   c(17) = 2    (a(17)=1, mask=true → 1*2)
!   c(32) = 32   (a(32)=16, mask=true → 16*2)
!
! CHECK:    -1.0000000E0    -1.0000000E0     2.0000000E0     3.2000000E1
program test_do_loop_vectorize_where_elsewhere_binop
  implicit none
  integer :: i
  real(4) :: a(32), c(32), two
  two = 2.0
  do i = 1, 32
    a(i) = real(i - 16, 4)
    c(i) = 0.0
  end do
  where (a > 0.0)
    c = a * two
  elsewhere
    c = -1.0
  end where
  print *, c(1), c(16), c(17), c(32)
end program test_do_loop_vectorize_where_elsewhere_binop
