! WHERE with a literal-constant store value (`b = K`). The matcher
! must accept a store value that is loop-invariant constant — no load,
! no unary, no binop in the then_block other than the Store itself.
! Lifts to vselect(mask, BroadcastK_vec, dest_vec) → store.
!
! a(i) = i - 16 (range -15..16); b(i) = i. Where a > 0 (lanes 17..32),
! set b = -1.0; otherwise leave b = i.
!   b(1)  = 1.0   (a(1)=-15, mask=false, b unchanged)
!   b(16) = 16.0  (a(16)=0, NOT > 0, mask=false)
!   b(17) = -1.0  (a(17)=1, mask=true, b ← -1.0)
!   b(32) = -1.0  (a(32)=16, mask=true, b ← -1.0)
!
! CHECK: 1.0000000E0     1.6000000E1    -1.0000000E0    -1.0000000E0
program test_do_loop_vectorize_where_const
  implicit none
  integer :: i
  real(4) :: a(32), b(32)
  do i = 1, 32
    a(i) = real(i - 16, 4)
    b(i) = real(i, 4)
  end do
  where (a > 0.0)
    b = -1.0
  end where
  print *, b(1), b(16), b(17), b(32)
end program test_do_loop_vectorize_where_const
