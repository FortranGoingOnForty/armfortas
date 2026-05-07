! i64 sum reduction. NeonVectorize uses V=2 lanes here, which means
! cross-lane reduce can't use `addv` (no `addv.2d` in NEON). Lower
! uses `addp.2d v_tmp, v_src, v_src` plus `umov.d x_dest, v_tmp[0]`.
!
! sum(1..32) = 528
! CHECK: 528
program test_do_loop_vectorize_reduce_i64
  implicit none
  integer :: i
  integer(8) :: a(32), s

  do i = 1, 32
    a(i) = int(i, 8)
  end do

  s = 0
  do i = 1, 32
    s = s + a(i)
  end do

  print *, s
end program test_do_loop_vectorize_reduce_i64
