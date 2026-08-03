! Invalid NAMELIST scalar and continuation values report failure without
! fabricating assignments, and a later valid read succeeds normally.
!
! FLAGS: --std=f2023
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_read_namelist
! IR_CHECK: call @afs_read_namelist_internal
program ar44_namelist_conversion_status
  implicit none

  character(len=128) :: record
  character(len=64) :: message
  integer :: unit
  integer :: ios
  integer :: integer_value
  integer :: array_value(2)
  real(kind=8) :: real_value
  logical :: logical_value

  namelist /cfg/ integer_value, real_value, logical_value, array_value

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(a)') '&cfg integer_value=not_an_integer /'
  rewind(unit)
  call reset_values()
  read(unit, nml=cfg, iostat=ios, iomsg=message)
  if (ios == 0) error stop 1
  if (integer_value /= 17) error stop 2
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 3
  close(unit)

  record = '&cfg real_value=not_a_real /'
  call reset_values()
  read(record, nml=cfg, iostat=ios, iomsg=message)
  if (ios == 0) error stop 4
  if (real_value /= 2.5_8) error stop 5
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 6

  record = '&cfg logical_value=maybe /'
  call reset_values()
  read(record, nml=cfg, iostat=ios, iomsg=message)
  if (ios == 0) error stop 7
  if (.not. logical_value) error stop 8
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 9

  record = '&cfg array_value=7,not_an_integer /'
  call reset_values()
  read(record, nml=cfg, iostat=ios, iomsg=message)
  if (ios == 0) error stop 10
  if (any(array_value /= [7, 22])) error stop 11
  if (len_trim(message) == 0 .or. trim(message) == 'sentinel') error stop 12

  record = '&cfg integer_value=42, real_value=1.25D+1, ' // &
       'logical_value=.FALSE., array_value=3,4 /'
  call reset_values()
  read(record, nml=cfg, iostat=ios, iomsg=message)
  if (ios /= 0) error stop 13
  if (integer_value /= 42) error stop 14
  if (real_value /= 12.5_8) error stop 15
  if (logical_value) error stop 16
  if (any(array_value /= [3, 4])) error stop 17

  print '(a)', 'ok'

contains

  subroutine reset_values()
    integer_value = 17
    real_value = 2.5_8
    logical_value = .true.
    array_value = [11, 22]
    ios = -99
    message = 'sentinel'
  end subroutine reset_values

end program ar44_namelist_conversion_status
