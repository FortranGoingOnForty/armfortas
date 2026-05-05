! Min/max reductions over `unary(load)` per element. The matcher
! lifts the unary into the vector lane (vabs/vneg) and the
! existing VMin / VMax + vreduce_max/min chain handles the rest.
!
! - L∞-norm (infinity norm): `max(s, abs(a(i)))`
! - Smallest magnitude:      `min(s, abs(a(i)))`
!
! For a(i) = i - 16 over i=1..32, the magnitudes range over
! 0..16 (with 0 appearing once at i=16). max(|.|) = 16,
! min(|.|) = 0.
program test_do_loop_vectorize_reduce_minmax_unary
  implicit none
  integer :: i
  real(4) :: a(32), maxabs, minabs

  do i = 1, 32
    a(i) = real(i - 16, 4)
  end do

  maxabs = 0.0
  do i = 1, 32
    maxabs = max(maxabs, abs(a(i)))
  end do

  minabs = 1000.0
  do i = 1, 32
    minabs = min(minabs, abs(a(i)))
  end do

  print *, maxabs
  print *, minabs
end program test_do_loop_vectorize_reduce_minmax_unary
