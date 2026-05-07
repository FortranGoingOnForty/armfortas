! Element-wise FMA fusion: `c(i) = a(i)*b(i) + d(i)` lifts the
! FMul + FAdd pair into a single VFma. The matcher accepts either
! orientation (`(a*b) + c` or `c + (a*b)`) and any of the three
! operands may be an invariant scalar (broadcast in preheader).
! NEON FMLA is float-only — i32 falls through to the regular sum
! path.
!
! c32(i) = i*2+1 -> c32(1)=3, c32(16)=33, c32(32)=65
! c64(i) = i*2+1 -> same values
! e32(i) = i*2.5+10 -> e32(4)=20
! CHECK: 3.0000000E0
! CHECK: 3.3000000E1
! CHECK: 6.5000000E1
! CHECK: 3.000000000000000E0
! CHECK: 3.300000000000000E1
! CHECK: 6.500000000000000E1
! CHECK: 2.0000000E1
program test_do_loop_vectorize_fma
  implicit none
  integer :: i
  real(4) :: a32(32), b32(32), d32(32), c32(32), e32(32)
  real(8) :: a64(32), b64(32), d64(32), c64(32)

  do i = 1, 32
    a32(i) = real(i, 4)
    b32(i) = 2.0
    d32(i) = 1.0
    a64(i) = real(i, 8)
    b64(i) = 2.0_8
    d64(i) = 1.0_8
  end do

  ! 3-load FMA, f32.
  do i = 1, 32
    c32(i) = a32(i) * b32(i) + d32(i)
  end do

  ! 3-load FMA, f64.
  do i = 1, 32
    c64(i) = a64(i) * b64(i) + d64(i)
  end do

  ! 1-load + 2 invariant scalar FMA, f32.
  do i = 1, 32
    e32(i) = a32(i) * 2.5 + 10.0
  end do

  print *, c32(1)
  print *, c32(16)
  print *, c32(32)
  print *, c64(1)
  print *, c64(16)
  print *, c64(32)
  print *, e32(4)
end program test_do_loop_vectorize_fma
