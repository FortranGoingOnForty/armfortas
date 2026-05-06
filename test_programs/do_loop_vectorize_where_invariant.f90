! WHERE with a loop-invariant scalar threshold (rather than a
! literal constant). The broadcast lifts the scalar into a
! preheader VBroadcast which feeds the per-iteration vfcmp.
!
! a(i) = i - 16 (range -15..16); b(i) = i; thresh = 5.0.
! After WHERE: lanes where a > 5 (i = 22..32) get b ← a.
!   b(1)  = 1   (a(1)=-15, mask=false)
!   b(20) = 20  (a(20)=4, mask=false)
!   b(21) = 21  (a(21)=5, NOT > 5, mask=false)
!   b(32) = 16  (a(32)=16, mask=true, b ← a = 16)
!
! CHECK: 1.0000000E0     2.0000000E1     2.1000000E1     1.6000000E1
program test_do_loop_vectorize_where_invariant
  implicit none
  integer :: i
  real(4) :: a(32), b(32), thresh
  thresh = 5.0
  do i = 1, 32
    a(i) = real(i - 16, 4)
    b(i) = real(i, 4)
  end do
  where (a > thresh)
    b = a
  end where
  print *, b(1), b(20), b(21), b(32)
end program test_do_loop_vectorize_where_invariant
