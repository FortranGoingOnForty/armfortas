! Regression (audit C1): a loop that carries BOTH an elementwise store
! and a reduction. The reduction vectorizer widened the IV to a vector
! stride but did not vectorize the `c(i) = a(i)*b(i)` store, so at
! -O3/-Ofast it ran once per vector iteration and dropped 3 of every 4
! lanes — c came out garbage while dot stayed correct (silent). The
! reduction vectorizer now fuses the store: it widens it to a VStore
! beside the vector reduction (every load/store is index-i-aligned, so
! there is no cross-lane or aliasing hazard). Correct at every opt
! level: c(i) = i * 2i, dot = sum of a(i)*b(i).
! See tests/vectorize_reduction_fused_store.rs for the vectorization guard.
!
! CHECK: c= 2 8 18 32 50 72 98 128
! CHECK: dot=408
program vec_reduction_with_elementwise_store
  implicit none
  integer, parameter :: n = 8
  integer :: a(n), b(n), c(n)
  integer :: i, dot
  do i = 1, n
    a(i) = i
    b(i) = 2 * i
  end do
  dot = 0
  do i = 1, n
    c(i) = a(i) * b(i)
    dot = dot + a(i) * b(i)
  end do
  print '(a,8(1x,i0))', 'c=', c
  print '(a,i0)', 'dot=', dot
end program vec_reduction_with_elementwise_store
