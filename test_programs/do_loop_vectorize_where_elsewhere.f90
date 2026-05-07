! WHERE / ELSEWHERE two-arm form. Both arms write to `b`. The matcher
! must recognize the diamond's else_block (currently empty in the
! else-less form) being non-empty, with a store to the same dest
! pointer. Lowered to vselect(mask, then_arm, else_arm) → store.
!
! a(i) = i - 16 (range -15..16); b(i) = 0.
! WHERE a > 0: b ← a (so 0..16 stays 0; 17..32 gets a = 1..16).
! ELSEWHERE: b ← -1.0 (so 0..16 becomes -1).
!   b(1)  = -1.0 (a(1)=-15, mask=false, b ← -1)
!   b(16) = -1.0 (a(16)=0, NOT > 0, mask=false, b ← -1)
!   b(17) =  1.0 (a(17)=1, mask=true, b ← a = 1)
!   b(32) = 16.0 (a(32)=16, mask=true, b ← a = 16)
!
! CHECK:    -1.0000000E0    -1.0000000E0     1.0000000E0     1.6000000E1
program test_do_loop_vectorize_where_elsewhere
  implicit none
  integer :: i
  real(4) :: a(32), b(32)
  do i = 1, 32
    a(i) = real(i - 16, 4)
    b(i) = 0.0
  end do
  where (a > 0.0)
    b = a
  elsewhere
    b = -1.0
  end where
  print *, b(1), b(16), b(17), b(32)
end program test_do_loop_vectorize_where_elsewhere
