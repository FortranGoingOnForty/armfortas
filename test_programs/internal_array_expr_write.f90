! CHECK: ok
! FLAGS: --std=f2023
! IR_CHECK: call @afs_deallocate_array(
program internal_array_expr_write
  implicit none

  character(len=64) :: fixed
  character(len=:), allocatable :: deferred
  integer :: values(4), result(4), ios, i

  values = [1, 2, 3, 4]
  do i = 1, 32
    fixed = ''
    ios = 77
    write(fixed, *, iostat=ios) values + 1
    if (ios /= 0) error stop 1
    result = 0
    ios = 77
    read(fixed, *, iostat=ios) result
    if (ios /= 0) error stop 2
    if (result(1) /= 2 .or. result(2) /= 3 .or. &
        result(3) /= 4 .or. result(4) /= 5) error stop 3

    ios = 77
    write(deferred, *, iostat=ios) values * 2
    if (ios /= 0) error stop 4
    result = 0
    ios = 77
    read(deferred, *, iostat=ios) result
    if (ios /= 0) error stop 5
    if (result(1) /= 2 .or. result(2) /= 4 .or. &
        result(3) /= 6 .or. result(4) /= 8) error stop 6
  end do

  print *, 'ok'
end program internal_array_expr_write
