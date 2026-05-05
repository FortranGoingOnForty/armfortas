! Manual integer max- and min-reductions. NeonVectorize Stage 5
! detects the `select(icmp ge|le acc, value, acc, value)` pattern
! that lowers `s = max(s, a(i))` and rewrites the body's
! select+icmp into a single vmax/vmin, then collapses the vector
! accumulator at exit via `vreduce_max` / `vreduce_min`. The
! codegen for those is `smaxv.4s` / `sminv.4s` plus `umov.s`.
!
! max(a) where a(i) = i for 1..32 → 32
! min(a) where a(i) = 100-i for 1..32 → 100-32 = 68
! CHECK: 32
! CHECK: 68
program test_do_loop_vectorize_reduce_minmax
  implicit none
  integer :: i, a(32), b(32), smax, smin

  do i = 1, 32
    a(i) = i
    b(i) = 100 - i
  end do

  smax = 0
  do i = 1, 32
    smax = max(smax, a(i))
  end do

  smin = 1000
  do i = 1, 32
    smin = min(smin, b(i))
  end do

  print *, smax
  print *, smin
end program test_do_loop_vectorize_reduce_minmax
