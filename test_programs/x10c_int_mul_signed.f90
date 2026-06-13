! x10c-3: signed i32 packed multiply. SSE2 has no pmulld (SSE4.1), so
! x86 synthesizes v4i32 multiply from two pmuludq (even/odd lanes) plus
! a shufps/pshufd merge — the gfortran/LLVM SSE2 lowering. arm64 uses
! native mul.4s. Low 32 bits of the product are identical for signed
! and unsigned, so negatives are fine; the lanes mix signs. OPT_EQ ties
! the vectorized O2+ output to the scalar O0 result.
! CHECK: c 21 6 -5 -12 -15 -14 -9 0 13 30 51 76 105 138 175 216
! CHECK: s 776
! CHECK: ok
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
program x10c_int_mul_signed
  implicit none
  integer, parameter :: n = 16
  integer :: a(n), b(n), c(n), i, s

  do i = 1, n
    a(i) = i - 8           ! -7 .. 8
    b(i) = 2 * i - 5       ! -3 .. 27
  end do

  ! Elementwise i32 multiply (folds to the pmuludq synthesis on x86).
  do i = 1, n
    c(i) = a(i) * b(i)
  end do

  ! Integer dot product (lane multiply then sum reduction).
  s = 0
  do i = 1, n
    s = s + a(i) * b(i)
  end do

  print '(A,16(1X,I0))', 'c', c
  print '(A,1X,I0)', 's', s
  print '(A)', 'ok'
end program x10c_int_mul_signed
