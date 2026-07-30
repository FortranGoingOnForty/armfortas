! DATE_AND_TIME assigns the leading characters of DATE, TIME, and ZONE
! when an actual is shorter than the complete representation. Zero-length
! and longer actuals exercise the neighboring assignment boundaries.
!
! FLAGS: --std=f2018
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_date_and_time_desc(
program ar53_date_and_time_truncation
  implicit none

  character(len=7) :: short_date
  character(len=9) :: short_time
  character(len=4) :: short_zone
  character(len=0) :: empty_date, empty_time, empty_zone
  character(len=10) :: long_date
  character(len=12) :: long_time
  character(len=7) :: long_zone
  character(len=8) :: expected_date
  character(len=10) :: expected_time
  character(len=5) :: expected_zone
  integer :: values(8)

  short_date = repeat('?', len(short_date))
  short_time = repeat('?', len(short_time))
  short_zone = repeat('?', len(short_zone))
  values = -huge(0)
  call date_and_time(short_date, short_time, short_zone, values)
  call make_expected(values, expected_date, expected_time, expected_zone)
  if (short_date /= expected_date(:len(short_date))) error stop 1
  if (short_time /= expected_time(:len(short_time))) error stop 2
  if (short_zone /= expected_zone(:len(short_zone))) error stop 3

  call date_and_time(empty_date, empty_time, empty_zone)

  long_date = repeat('?', len(long_date))
  long_time = repeat('?', len(long_time))
  long_zone = repeat('?', len(long_zone))
  values = -huge(0)
  call date_and_time(long_date, long_time, long_zone, values)
  call make_expected(values, expected_date, expected_time, expected_zone)
  if (long_date /= expected_date // '  ') error stop 4
  if (long_time /= expected_time // '  ') error stop 5
  if (long_zone /= expected_zone // '  ') error stop 6

  print '(a)', 'ok'
contains
  subroutine make_expected(snapshot, date, time, zone)
    integer, intent(in) :: snapshot(8)
    character(len=8), intent(out) :: date
    character(len=10), intent(out) :: time
    character(len=5), intent(out) :: zone
    character :: sign
    integer :: zone_minutes

    if (snapshot(1) < 0 .or. snapshot(2) < 1 .or. snapshot(3) < 1) error stop 7
    if (snapshot(4) < -1440 .or. snapshot(4) > 1440) error stop 8
    if (snapshot(5) < 0 .or. snapshot(5) > 23) error stop 9
    if (snapshot(6) < 0 .or. snapshot(6) > 59) error stop 10
    if (snapshot(7) < 0 .or. snapshot(7) > 60) error stop 11
    if (snapshot(8) < 0 .or. snapshot(8) > 999) error stop 12

    write(date, '(i4.4,i2.2,i2.2)') snapshot(1), snapshot(2), snapshot(3)
    write(time, '(i2.2,i2.2,i2.2,a1,i3.3)') &
      snapshot(5), snapshot(6), snapshot(7), '.', snapshot(8)
    sign = '+'
    if (snapshot(4) < 0) sign = '-'
    zone_minutes = abs(snapshot(4))
    write(zone, '(a1,i2.2,i2.2)') sign, zone_minutes / 60, mod(zone_minutes, 60)
  end subroutine make_expected
end program ar53_date_and_time_truncation
