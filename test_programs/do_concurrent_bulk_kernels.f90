! DO CONCURRENT full-array maps should collapse into bulk kernels when the
! body is a clean elementwise map over the full array extent.
!
! CHECK: 78
! CHECK: 12
! CHECK: -76
! CHECK: 176
! CHECK: 76
! IR_CHECK: call @afs_array_add_i32(
! IR_CHECK: call @afs_array_mul_scalar_i32(
! IR_CHECK: call @afs_array_sub_scalar_i32(
! IR_CHECK: call @afs_scalar_sub_array_i32(
! IR_NOT: doconc_check_
! ASM_CHECK: _afs_array_add_i32
! ASM_CHECK(x86_64-freebsd): afs_array_add_i32
! ASM_CHECK(x86_64-linux-gnu): afs_array_add_i32
! ASM_CHECK: _afs_array_mul_scalar_i32
! ASM_CHECK(x86_64-freebsd): afs_array_mul_scalar_i32
! ASM_CHECK(x86_64-linux-gnu): afs_array_mul_scalar_i32
! ASM_CHECK: _afs_array_sub_scalar_i32
! ASM_CHECK(x86_64-freebsd): afs_array_sub_scalar_i32
! ASM_CHECK(x86_64-linux-gnu): afs_array_sub_scalar_i32
! ASM_CHECK: _afs_scalar_sub_array_i32
! ASM_CHECK(x86_64-freebsd): afs_scalar_sub_array_i32
! ASM_CHECK(x86_64-linux-gnu): afs_scalar_sub_array_i32
program test_do_concurrent_bulk_kernels
  implicit none
  integer :: i, a(8), b(8), c(8)

  do i = 1, 8
    a(i) = i
    b(i) = i * 10
  end do

  do concurrent (i = 1:8)
    c(i) = a(i) + b(i)
  end do
  do concurrent (i = 1:8)
    a(i) = c(i) * 2
  end do
  do concurrent (i = 1:8)
    b(i) = b(i) - 4
  end do
  do concurrent (i = 1:8)
    c(i) = 100 - a(i)
  end do

  print *, c(1)
  print *, c(4)
  print *, c(8)
  print *, a(8)
  print *, b(8)
end program test_do_concurrent_bulk_kernels
