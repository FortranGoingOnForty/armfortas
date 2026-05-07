! Element-wise sqrt over a load: c(i) = sqrt(a(i)).
! Lifts to VSqrt and lowers to fsqrt.4s / fsqrt.2d.
!
! CHECK: 1.0000000E0
! CHECK: 2.0000000E0
! CHECK: 4.0000000E0
! CHECK: 1.000000000000000E0
! CHECK: 2.000000000000000E0
! CHECK: 4.000000000000000E0
program test_do_loop_vectorize_sqrt
  implicit none
  integer :: i
  real(4) :: a32(32), c32(32)
  real(8) :: a64(32), c64(32)

  do i = 1, 32
    a32(i) = real(i, 4)
    a64(i) = real(i, 8)
  end do

  do i = 1, 32
    c32(i) = sqrt(a32(i))
  end do

  do i = 1, 32
    c64(i) = sqrt(a64(i))
  end do

  ! Print a couple of representative results.
  print *, c32(1)
  print *, c32(4)
  print *, c32(16)
  print *, c64(1)
  print *, c64(4)
  print *, c64(16)
end program test_do_loop_vectorize_sqrt
