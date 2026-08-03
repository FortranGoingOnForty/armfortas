! DATE_AND_TIME VALUES must honor the actual integer kind and descriptor
! stride. The wide array exposes fixed-i32 stores; the guarded section
! exposes contiguous writes through a noncontiguous actual.
!
! FLAGS: --std=f2018
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_date_and_time_desc(
program ar44_date_and_time_values
  use iso_fortran_env, only: int32, int64
  implicit none

  integer(int64), parameter :: sentinel64 = -huge(0_int64)
  integer(int32), parameter :: sentinel32 = -huge(0_int32)
  integer(int64) :: wide(8)
  integer(int32) :: guarded(17)
  character(len=8) :: date
  character(len=10) :: time
  character(len=5) :: zone
  integer :: i

  wide = sentinel64
  date = ''
  time = ''
  zone = ''
  call date_and_time(date, time, zone, wide)
  if (len_trim(date) /= len(date)) error stop 20
  if (len_trim(time) /= len(time)) error stop 21
  if (len_trim(zone) /= len(zone)) error stop 22
  if (any(wide == sentinel64)) error stop 1
  if (wide(1) < 1970_int64 .or. wide(1) > 9999_int64) error stop 2
  if (wide(2) < 1_int64 .or. wide(2) > 12_int64) error stop 3
  if (wide(3) < 1_int64 .or. wide(3) > 31_int64) error stop 4
  if (wide(4) < -1440_int64 .or. wide(4) > 1440_int64) error stop 5
  if (wide(5) < 0_int64 .or. wide(5) > 23_int64) error stop 6
  if (wide(6) < 0_int64 .or. wide(6) > 59_int64) error stop 7
  if (wide(7) < 0_int64 .or. wide(7) > 60_int64) error stop 8
  if (wide(8) < 0_int64 .or. wide(8) > 999_int64) error stop 9

  guarded = sentinel32
  call date_and_time(values=guarded(16:2:-2))
  do i = 1, 17, 2
    if (guarded(i) /= sentinel32) error stop 10
  end do
  do i = 2, 16, 2
    if (guarded(i) == sentinel32) error stop 11
  end do
  if (guarded(16) < 1970_int32 .or. guarded(16) > 9999_int32) error stop 12
  if (guarded(14) < 1_int32 .or. guarded(14) > 12_int32) error stop 13
  if (guarded(12) < 1_int32 .or. guarded(12) > 31_int32) error stop 14
  if (guarded(10) < -1440_int32 .or. guarded(10) > 1440_int32) error stop 15
  if (guarded(8) < 0_int32 .or. guarded(8) > 23_int32) error stop 16
  if (guarded(6) < 0_int32 .or. guarded(6) > 59_int32) error stop 17
  if (guarded(4) < 0_int32 .or. guarded(4) > 60_int32) error stop 18
  if (guarded(2) < 0_int32 .or. guarded(2) > 999_int32) error stop 19

  print '(a)', 'ok'
end program ar44_date_and_time_values
