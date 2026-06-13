! x10c-2: signed i32 packed abs. Integer ABS lowers to
! select(x>=0, x, -x); the vectorizer folds that idiom to a unary abs
! so the backend emits the dedicated synthesis instead of scalarizing
! (x86: sign-mask pcmpgtd + pxor + psubd, the gfortran/LLVM SSE2 form;
! arm64: abs.4s). Negatives across the lanes exercise the sign path.
! OPT_EQ ties the vectorized O2+ output to the scalar O0 result.
! CHECK: c 14 26 19 12 24 17 10 3 4 8 1 6 6 1 8 4
! CHECK: s 163
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program x10c_int_abs_signed
  implicit none
  integer, parameter :: n = 16
  integer :: a(n), c(n), i, s

  do i = 1, n
    a(i) = mod(i * 7 - 50, 19) - 9      ! spread of negatives and positives
  end do

  ! Elementwise abs (folds to packed abs).
  do i = 1, n
    c(i) = abs(a(i))
  end do

  ! Sum-of-abs reduction (abs inside a sum reduction).
  s = 0
  do i = 1, n
    s = s + abs(a(i))
  end do

  print '(A,16(1X,I0))', 'c', c
  print '(A,1X,I0)', 's', s
  print '(A)', 'ok'
end program x10c_int_abs_signed
