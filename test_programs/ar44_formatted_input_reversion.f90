! Formatted input reverts at the rightmost parenthesized group, starts
! each finite-format reversion on a new record, and preserves input
! editing state established before the reversion point.
!
! FLAGS: --std=f2023
! CHECK: 1 2 3 4 5 6 7 8 9
! CHECK: 102 304
! CHECK: 1 2 3
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_fmt_read_int
program ar44_formatted_input_reversion
  implicit none

  integer :: unit
  integer :: ios
  integer :: values(9)
  integer :: unlimited_values(3)
  integer :: short_values(3)
  real :: blank_zero_values(2)

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(A)') ' 0102 03'
  write(unit, '(A)') '0405 06'
  write(unit, '(A)') '0708 09'
  rewind(unit)
  values = -1
  read(unit, '(1X,2(I2),1X,I2)', iostat=ios) values
  if (ios /= 0) error stop 1
  if (any(values /= [1, 2, 3, 4, 5, 6, 7, 8, 9])) error stop 2
  close(unit)

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(A)') ' 1 2'
  write(unit, '(A)') '3 4'
  rewind(unit)
  blank_zero_values = -1.0
  read(unit, '(BZ,1X,(F3.0))', iostat=ios) blank_zero_values
  if (ios /= 0) error stop 3
  if (any(abs(blank_zero_values - [102.0, 304.0]) > 0.001)) error stop 4
  close(unit)

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(A)') '010203'
  rewind(unit)
  unlimited_values = -1
  read(unit, '(*(I2))', iostat=ios) unlimited_values
  if (ios /= 0) error stop 5
  if (any(unlimited_values /= [1, 2, 3])) error stop 6
  close(unit)

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(A)') '01'
  write(unit, '(A)') '02'
  rewind(unit)
  short_values = -1
  read(unit, '(I2)', iostat=ios) short_values
  if (ios >= 0) error stop 7
  if (any(short_values /= [1, 2, -1])) error stop 8
  close(unit)

  print *, values
  print *, int(blank_zero_values(1)), int(blank_zero_values(2))
  print *, unlimited_values
end program ar44_formatted_input_reversion
