! Floating-point sum reductions. NEON has no `faddv` for f32 or f64,
! so VReduceSum on Float lowers via two-step pairwise add:
!   - f32: `faddp.4s v_tmp, v_src, v_src` then `faddp.2s s_dest, v_tmp`
!   - f64: `faddp.2d d_dest, v_src` (single step is enough — only 2 lanes)
!
! Both should sum 1..32 and print 528.0.
! CHECK: 5.2800000E2
! CHECK: 5.280000000000000E2
program test_do_loop_vectorize_reduce_fp
  implicit none
  integer :: i
  real(4) :: a32(32), s32
  real(8) :: a64(32), s64

  do i = 1, 32
    a32(i) = real(i, 4)
    a64(i) = real(i, 8)
  end do

  s32 = 0.0
  do i = 1, 32
    s32 = s32 + a32(i)
  end do

  s64 = 0.0_8
  do i = 1, 32
    s64 = s64 + a64(i)
  end do

  print *, s32
  print *, s64
end program test_do_loop_vectorize_reduce_fp
