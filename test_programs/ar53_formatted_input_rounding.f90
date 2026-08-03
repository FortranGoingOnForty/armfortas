! Formatted real input must apply RU/RD at the destination kind.  The
! single-precision cases are deliberately exact binary midpoints so that
! reading through a double temporary cannot accidentally satisfy the test.
!
! FLAGS: --std=f2018
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_fmt_read_real32_internal(
! IR_CHECK: call @afs_fmt_read_real_internal(
! IR_CHECK: call @afs_fmt_read_real32(
program ar53_formatted_input_rounding
  use iso_fortran_env, only: int32, int64, real32, real64
  implicit none

  character(len=26) :: positive32
  character(len=27) :: negative32
  character(len=55) :: positive64
  character(len=56) :: negative64
  character(len=106) :: internal32
  character(len=25) :: implied32
  character(len=26) :: scaled32
  integer :: unit, ios
  real(real32) :: edge32, values32(4), reverted32(2)
  real(real64) :: values64(4)

  positive32 = '1.000000059604644775390625'
  negative32 = '-1.000000059604644775390625'
  positive64 = '1.00000000000000011102230246251565404236316680908203125'
  negative64 = '-1.00000000000000011102230246251565404236316680908203125'
  implied32 = '1000000059604644775390625'
  scaled32 = '10.00000059604644775390625'

  internal32 = positive32 // positive32 // negative32 // negative32
  read(internal32, '(RU,(F26.24),RD,F26.24,RU,F27.24,RD,F27.24)', iostat=ios) values32
  if (ios /= 0) error stop 1
  call check32(values32(1), int(z'3F800001', int32), 2)
  call check32(values32(2), int(z'3F800000', int32), 3)
  call check32(values32(3), int(z'BF800000', int32), 4)
  call check32(values32(4), int(z'BF800001', int32), 5)

  read(positive64, '(RU,F55.53)', iostat=ios) values64(1)
  if (ios /= 0) error stop 6
  read(positive64, '(RD,F55.53)', iostat=ios) values64(2)
  if (ios /= 0) error stop 7
  read(negative64, '(RU,F56.53)', iostat=ios) values64(3)
  if (ios /= 0) error stop 8
  read(negative64, '(RD,F56.53)', iostat=ios) values64(4)
  if (ios /= 0) error stop 9
  call check64(values64(1), int(z'3FF0000000000001', int64), 10)
  call check64(values64(2), int(z'3FF0000000000000', int64), 11)
  call check64(values64(3), -4616189618054758400_int64, 12)
  call check64(values64(4), -4616189618054758399_int64, 13)

  read(positive32, '(RN,F26.24)', iostat=ios) edge32
  if (ios /= 0) error stop 14
  call check32(edge32, int(z'3F800000', int32), 15)
  read(negative32, '(RZ,F27.24)', iostat=ios) edge32
  if (ios /= 0) error stop 16
  call check32(edge32, int(z'BF800000', int32), 17)
  read(implied32, '(RU,F25.24)', iostat=ios) edge32
  if (ios /= 0) error stop 18
  call check32(edge32, int(z'3F800001', int32), 19)
  read(scaled32, '(1P,RD,F26.24)', iostat=ios) edge32
  if (ios /= 0) error stop 20
  call check32(edge32, int(z'3F800000', int32), 21)

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(A)') positive32
  write(unit, '(A)') positive32
  rewind(unit)
  reverted32 = -1.0_real32
  read(unit, '(RU,(F26.24))', iostat=ios) reverted32
  close(unit)
  if (ios /= 0) error stop 22
  call check32(reverted32(1), int(z'3F800001', int32), 23)
  call check32(reverted32(2), int(z'3F800001', int32), 24)

  print '(a)', 'ok'

contains

  subroutine check32(value, expected, code)
    real(real32), intent(in) :: value
    integer(int32), intent(in) :: expected
    integer, intent(in) :: code
    if (transfer(value, 0_int32) /= expected) error stop code
  end subroutine check32

  subroutine check64(value, expected, code)
    real(real64), intent(in) :: value
    integer(int64), intent(in) :: expected
    integer, intent(in) :: code
    if (transfer(value, 0_int64) /= expected) error stop code
  end subroutine check64

end program ar53_formatted_input_rounding
