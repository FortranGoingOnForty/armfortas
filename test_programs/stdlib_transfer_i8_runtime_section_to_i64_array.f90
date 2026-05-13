! CHECK: ok
! IR_CHECK: call @afs_create_section(
! IR_CHECK: call @memcpy(
! REPRO_CHECK: run
program stdlib_transfer_i8_runtime_section_to_i64_array
  use iso_fortran_env, only: int8, int64
  implicit none

  integer(int8) :: key(64)
  integer :: i

  do i = 1, 64
    key(i) = int(i, int8)
  end do

  call probe(key(1:32))
  print *, 'ok'

contains
  subroutine probe(x)
    integer(int8), intent(in) :: x(0:)
    integer(int64) :: whole(0:3), sliced(0:3)
    integer(int64) :: index

    whole = -1_int64
    sliced = -1_int64
    index = 0_int64

    whole = transfer(x(0:31), 0_int64, 4)
    sliced(0:3) = transfer(x(index:index+31), 0_int64, 4)

    if (whole(0) /= int(z'0807060504030201', int64)) error stop 1
    if (whole(1) /= int(z'100f0e0d0c0b0a09', int64)) error stop 2
    if (whole(2) /= int(z'1817161514131211', int64)) error stop 3
    if (whole(3) /= int(z'201f1e1d1c1b1a19', int64)) error stop 4
    if (sliced(0) /= int(z'0807060504030201', int64)) error stop 5
    if (sliced(1) /= int(z'100f0e0d0c0b0a09', int64)) error stop 6
    if (sliced(2) /= int(z'1817161514131211', int64)) error stop 7
    if (sliced(3) /= int(z'201f1e1d1c1b1a19', int64)) error stop 8
  end subroutine probe
end program stdlib_transfer_i8_runtime_section_to_i64_array
