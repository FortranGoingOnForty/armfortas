! Whole-array bulk kernels: fixed-size broadcast and array addition.
!
! CHECK: 11
! CHECK: 44
! CHECK: 88
! CHECK: 7
! CHECK: 7
! IR_CHECK: call @afs_array_add_i32(
! IR_CHECK: call @afs_fill_i32(
! ASM_CHECK: _afs_array_add_i32
! ASM_CHECK(x86_64-freebsd): afs_array_add_i32
! ASM_CHECK(x86_64-linux-gnu): afs_array_add_i32
! ASM_CHECK: _afs_fill_i32
! ASM_CHECK(x86_64-freebsd): afs_fill_i32
! ASM_CHECK(x86_64-linux-gnu): afs_fill_i32
program test_array_bulk_kernels
  implicit none
  integer :: a(8), b(8), c(8)
  integer :: i

  do i = 1, 8
    a(i) = i
    b(i) = i * 10
  end do

  c = a + b
  a = 7

  print *, c(1)
  print *, c(4)
  print *, c(8)
  print *, a(1)
  print *, a(8)
end program test_array_bulk_kernels
