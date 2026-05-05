! Min/max reductions with non-divisible trip counts vectorize the
! head and peel the remaining iterations into the exit block. The
! peel walks the body snapshot, which preserves the scalar
! `select(cmp acc, value, acc, value)` pattern; with `acc_param`
! pre-mapped to the running scalar acc, each peel produces a
! correct chained select that the VMax/VMin replacement in the
! head doesn't touch.
!
! Trip = 31, V = 4 (i32) → head 28 + tail 3.
! Trip = 31, V = 4 (f32) → head 28 + tail 3.
! Trip = 31, V = 2 (f64) → head 30 + tail 1.
!
! mx32 = max(100-i, 1..31) = 99; mn32 = min(...) = 69;
! rmx32 = 99.0; rmn64 = min(i-100, 1..31) = -99.0.
! CHECK: 99
! CHECK: 69
! CHECK: 9.9000000E1
! CHECK: -9.900000000000000E1
program test_do_loop_vectorize_reduce_minmax_tail
  implicit none
  integer :: i, mn32, mx32
  real(4) :: rmx32
  real(8) :: rmn64
  integer :: a(31)
  real(4) :: f32(31)
  real(8) :: f64(31)

  do i = 1, 31
    a(i) = 100 - i
    f32(i) = real(100 - i, 4)
    f64(i) = real(i - 100, 8)
  end do

  mx32 = -1
  do i = 1, 31
    mx32 = max(mx32, a(i))
  end do

  mn32 = 1000
  do i = 1, 31
    mn32 = min(mn32, a(i))
  end do

  rmx32 = 0.0
  do i = 1, 31
    rmx32 = max(rmx32, f32(i))
  end do

  rmn64 = 0.0_8
  do i = 1, 31
    rmn64 = min(rmn64, f64(i))
  end do

  print *, mx32
  print *, mn32
  print *, rmx32
  print *, rmn64
end program test_do_loop_vectorize_reduce_minmax_tail
