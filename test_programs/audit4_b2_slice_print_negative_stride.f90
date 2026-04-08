! Audit #4 BLOCKING-2 — 1-D slice print with negative stride.
!
! Fixed: lower_1d_slice_write now mirrors the sign-of-step
! handling from the regular DO lowerer. The previous hardcoded
! `Gt` exit check made `print *, a(5:1:-1)` exit before writing
! anything; now compile-time and runtime negative strides both
! iterate correctly.
!
! CHECK: 50 40 30 20 10
program audit4_b2_slice_print_negative_stride
  integer :: a(5) = [10, 20, 30, 40, 50]
  print *, a(5:1:-1)
end program
