! CHECK: ok
! IR_CHECK: call @afs_iolength_add(
! IR_CHECK: call @afs_iolength_add_array(
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|clean|repro
program inquire_iolength
  use, intrinsic :: iso_fortran_env, only: int8, int16, int32, int64, real32, real64
  implicit none

  type :: record_t
    integer(int8) :: tag
    integer(int64) :: payload
    character(3) :: code
  end type record_t

  integer(int8) :: i1
  integer(int16) :: i2
  integer(int32) :: i4
  integer(int64) :: i8
  integer(16) :: i16
  real(real32) :: r4
  real(real64) :: r8
  complex(real32) :: c4
  complex(real64) :: c8
  logical(1) :: l1
  logical(2) :: l2
  logical(4) :: l4
  logical(8) :: l8
  logical(16) :: l16
  character(3) :: text
  character(:), allocatable :: dynamic
  character(4), allocatable :: words(:)
  integer(int16) :: small(2, 3)
  integer(int32), allocatable :: large(:)
  type(record_t) :: record
  type(record_t) :: records(2)
  type(record_t), allocatable :: large_records(:)
  integer(int8) :: length1
  integer(int16) :: length2
  integer :: actual_size, calls, io_status, j, n, unit
  integer(int64) :: j64, n64
  integer(16) :: length16

  n = -777
  inquire(iolength=n) i1, i2, i4, i8, i16, r4, r8, c4, c8, &
    l1, l2, l4, l8, l16, text
  if (n /= 101) error stop 1
  open(newunit=unit, status='scratch', access='stream', form='unformatted', &
    iostat=io_status)
  if (io_status /= 0) error stop 10
  write(unit, iostat=io_status) i1, i2, i4, i8, i16, r4, r8, c4, c8, &
    l1, l2, l4, l8, l16, text
  if (io_status /= 0) error stop 11
  inquire(unit=unit, size=actual_size)
  if (actual_size /= n) error stop 12
  close(unit)

  inquire(iolength=length1) i1
  inquire(iolength=length2) c8
  inquire(iolength=length16) i1, i2, i4, i8, i16
  if (length1 /= 1 .or. length2 /= 16 .or. length16 /= 31) error stop 14

  n = -777
  inquire(iolength=n) (i1, j64=2147483647_int64, 2147483648_int64)
  if (n /= 2) error stop 15

  allocate(words(3), large(1000000))
  n64 = -777_int64
  inquire(iolength=n64) small, words, large
  if (n64 /= 4000024_int64) error stop 2

  deallocate(large)
  allocate(large(0))
  n = -777
  inquire(iolength=n) large
  if (n /= 0) error stop 3

  dynamic = 'dynamic'
  n64 = -777_int64
  inquire(iolength=n64) dynamic // '!'
  if (n64 /= 8_int64) error stop 4

  n = -777
  inquire(iolength=n) record, records
  if (n /= 36) error stop 5

  allocate(large_records(1000000))
  n64 = -777_int64
  inquire(iolength=n64) large_records
  if (n64 /= 12000000_int64) error stop 17
  deallocate(large_records)

  n = -777
  inquire(iolength=n) small(:, 2:3)
  if (n /= 8) error stop 6

  n = -777
  inquire(iolength=n) small + 1_int16
  if (n /= 12) error stop 13

  n = -777
  inquire(iolength=n) (i4, j=5, 1, -2)
  if (n /= 12) error stop 7

  n = -777
  inquire(iolength=n) (i4, j=1, 5, -1)
  if (n /= 0) error stop 8

  calls = 0
  n = -777
  inquire(iolength=n) next_value(), next_value()
  if (n /= 8 .or. calls /= 2) error stop 9

  print '(a)', 'ok'

contains

  integer(int32) function next_value()
    calls = calls + 1
    next_value = calls
  end function next_value

end program inquire_iolength
