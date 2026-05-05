! Floating-point min/max reductions over real(4) and real(8) arrays.
!   - f32 max: `fmaxv.4s s_dest, v_src` (single across-lane reduce)
!   - f32 min: `fminv.4s s_dest, v_src`
!   - f64 max: `fmaxp.2d d_dest, v_src` (NEON has no `fmaxv.2d`;
!     the pairwise scalar form is the across-lane reduce for 2 lanes)
!   - f64 min: `fminp.2d d_dest, v_src`
!
! Inputs are 1.0..32.0; max should be 32.0, min should be 1.0.
! CHECK: 3.2000000E1
! CHECK: 1.0000000
! CHECK: 3.200000000000000E1
! CHECK: 1.000000000000000
program test_do_loop_vectorize_reduce_fp_minmax
  implicit none
  integer :: i
  real(4) :: a32(32), mx32, mn32
  real(8) :: a64(32), mx64, mn64

  do i = 1, 32
    a32(i) = real(i, 4)
    a64(i) = real(i, 8)
  end do

  mx32 = 0.0
  do i = 1, 32
    mx32 = max(mx32, a32(i))
  end do

  mn32 = 100.0
  do i = 1, 32
    mn32 = min(mn32, a32(i))
  end do

  mx64 = 0.0_8
  do i = 1, 32
    mx64 = max(mx64, a64(i))
  end do

  mn64 = 100.0_8
  do i = 1, 32
    mn64 = min(mn64, a64(i))
  end do

  print *, mx32
  print *, mn32
  print *, mx64
  print *, mn64
end program test_do_loop_vectorize_reduce_fp_minmax
