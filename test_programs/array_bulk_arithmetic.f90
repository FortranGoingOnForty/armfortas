! Whole-array bulk arithmetic kernels: array-array sub, array-scalar mul/add,
! and scalar-array sub.
!
! CHECK: 87
! CHECK: 33
! CHECK: -39
! CHECK: 144
! CHECK: -8
! IR_CHECK: call @afs_array_sub_i32(
! IR_CHECK: call @afs_array_mul_scalar_i32(
! IR_CHECK: call @afs_scalar_sub_array_i32(
! IR_CHECK: call @afs_array_add_scalar_i32(
! ASM_CHECK: _afs_array_sub_i32
! ASM_CHECK(x86_64-freebsd): afs_array_sub_i32
! ASM_CHECK(x86_64-linux-gnu): afs_array_sub_i32
! ASM_CHECK: _afs_array_mul_scalar_i32
! ASM_CHECK(x86_64-freebsd): afs_array_mul_scalar_i32
! ASM_CHECK(x86_64-linux-gnu): afs_array_mul_scalar_i32
! ASM_CHECK: _afs_scalar_sub_array_i32
! ASM_CHECK(x86_64-freebsd): afs_scalar_sub_array_i32
! ASM_CHECK(x86_64-linux-gnu): afs_scalar_sub_array_i32
! ASM_CHECK: _afs_array_add_scalar_i32
! ASM_CHECK(x86_64-freebsd): afs_array_add_scalar_i32
! ASM_CHECK(x86_64-linux-gnu): afs_array_add_scalar_i32
program test_array_bulk_arithmetic
  implicit none
  integer :: a(8), b(8), c(8)
  integer :: i

  do i = 1, 8
    a(i) = i
    b(i) = i * 10
  end do

  c = b - a
  a = c * 2
  b = 100 - a
  c = b + 5

  print *, c(1)
  print *, c(4)
  print *, c(8)
  print *, a(8)
  print *, b(6)
end program test_array_bulk_arithmetic
