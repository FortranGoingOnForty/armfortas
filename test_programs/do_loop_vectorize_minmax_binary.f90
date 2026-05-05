! Element-wise binary max / min over two array loads:
! `c(i) = max(a(i), b(i))` and `c(i) = min(a(i), b(i))`. The IR
! shape is `select(cmp(la, lb), t, f)` where {t, f} = {la, lb};
! the matcher lifts both loads to vload and rewrites select to
! VMax/VMin. Tested for i32, f32, f64.
program test_do_loop_vectorize_minmax_binary
  implicit none
  integer :: i
  integer :: ai(32), bi(32), ci(32), di(32)
  real(4) :: a32(32), b32(32), c32(32), d32(32)
  real(8) :: a64(32), b64(32), c64(32), d64(32)

  do i = 1, 32
    ai(i) = i
    bi(i) = 33 - i
    a32(i) = real(i, 4)
    b32(i) = real(33 - i, 4)
    a64(i) = real(i, 8)
    b64(i) = real(33 - i, 8)
  end do

  do i = 1, 32
    ci(i) = max(ai(i), bi(i))
  end do
  do i = 1, 32
    di(i) = min(ai(i), bi(i))
  end do

  do i = 1, 32
    c32(i) = max(a32(i), b32(i))
  end do
  do i = 1, 32
    d32(i) = min(a32(i), b32(i))
  end do

  do i = 1, 32
    c64(i) = max(a64(i), b64(i))
  end do
  do i = 1, 32
    d64(i) = min(a64(i), b64(i))
  end do

  ! Spot-check four lanes: i=1 / 16 / 17 / 32.
  print *, ci(1), ci(16), ci(17), ci(32)
  print *, di(1), di(16), di(17), di(32)
  print *, c32(1), c32(16), c32(17), c32(32)
  print *, d32(1), d32(16), d32(17), d32(32)
  print *, c64(1), c64(16), c64(17), c64(32)
  print *, d64(1), d64(16), d64(17), d64(32)
end program test_do_loop_vectorize_minmax_binary
