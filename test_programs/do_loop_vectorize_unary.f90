! Unary element-wise body — `a(i) = -b(i)` and `a(i) = abs(b(i))`.
! NeonVectorize should rewrite Load → VLoad and the FNeg/FAbs
! into VNeg/VAbs.
!
! CHECK: 1.5000000E1
! CHECK: -1.6000000E1
! CHECK: 1.5000000E1
! CHECK: 0.0000000E0
! CHECK: 1.6000000E1
program test_do_loop_vectorize_unary
  implicit none
  integer :: i
  real(4) :: n(32), a(32), b(32)

  do i = 1, 32
    b(i) = real(i, 4) - 16.0
  end do

  do i = 1, 32
    n(i) = -b(i)
  end do

  do i = 1, 32
    a(i) = abs(b(i))
  end do

  print *, n(1)   ! -(-15) = 15? No: b(1) = 1-16 = -15, n(1) = -(-15) = 15
  print *, n(32)  ! b(32) = 32-16 = 16, n(32) = -16
  print *, a(1)   ! abs(-15) = 15
  print *, a(16)  ! abs(0) = 0
  print *, a(32)  ! abs(16) = 16
end program test_do_loop_vectorize_unary
