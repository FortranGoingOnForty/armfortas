! Vectorize a Fortran WHERE block over a counted loop.
! `where (a > 0.0); b = a; end where` lowers to a 4-block diamond
! (header → body cmp → {then store, skip} → incr). The vectorizer
! detects this shape and rewrites it into a single straight-line
! body that uses `vload a, vload b_old, vfcmp, vselect, vstore b`,
! with the `then` block dropped.
!
! a(i) = i - 16 → values [-15..16]; b(i) = i → [1..32].
! After WHERE: lanes where a > 0 (i = 17..32) get b = a; the rest
! keep their original b. So:
!   b(1)  = 1  (a(1) = -15, mask=false, unchanged)
!   b(16) = 16 (a(16) = 0,  mask=false, unchanged)
!   b(17) = 1  (a(17) = 1,  mask=true,  b ← a = 1)
!   b(32) = 16 (a(32) = 16, mask=true,  b ← a = 16)
!
! CHECK: 1.0000000E0     1.6000000E1     1.0000000E0     1.6000000E1
program test_do_loop_vectorize_where
  implicit none
  integer :: i
  real(4) :: a(32), b(32)
  do i = 1, 32
    a(i) = real(i - 16, 4)
    b(i) = real(i, 4)
  end do
  where (a > 0.0)
    b = a
  end where
  print *, b(1), b(16), b(17), b(32)
end program test_do_loop_vectorize_where
