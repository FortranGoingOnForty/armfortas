! NAMELIST group termination ignores slashes inside character literals for
! external and internal reads, including doubled delimiters.
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
program ar44_namelist_quoted_slash
  implicit none

  character(len=32) :: path
  character(len=32) :: note
  character(len=32) :: escaped
  character(len=160) :: record
  integer :: unit
  integer :: ios
  integer :: value

  namelist /cfg/ path, note, escaped, value

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(a)') '&cfg'
  write(unit, '(a)') " path='left/right'"
  write(unit, '(a)') ' note="up/down"'
  write(unit, '(a)') " escaped='say ''/'' now'"
  write(unit, '(a)') ' value=42'
  write(unit, '(a)') '/'
  write(unit, '(a)') "&cfg path='second/path', note='next/value', " // &
       "escaped='still / data', value=84 /"
  rewind(unit)

  call reset_values()
  read(unit, nml=cfg, iostat=ios)
  if (ios /= 0) error stop 1
  if (path /= 'left/right') error stop 2
  if (note /= 'up/down') error stop 3
  if (escaped /= "say '/' now") error stop 4
  if (value /= 42) error stop 5

  call reset_values()
  read(unit, nml=cfg, iostat=ios)
  if (ios /= 0) error stop 6
  if (path /= 'second/path') error stop 7
  if (note /= 'next/value') error stop 8
  if (escaped /= 'still / data') error stop 9
  if (value /= 84) error stop 10
  close(unit)

  record = '&cfg path="internal/a", note=''internal/b'', ' // &
       'escaped="say ""/"" now", value=77 /'
  call reset_values()
  read(record, nml=cfg, iostat=ios)
  if (ios /= 0) error stop 11
  if (path /= 'internal/a') error stop 12
  if (note /= 'internal/b') error stop 13
  if (escaped /= 'say "/" now') error stop 14
  if (value /= 77) error stop 15

  print '(a)', 'ok'

contains

  subroutine reset_values()
    path = 'sentinel'
    note = 'sentinel'
    escaped = 'sentinel'
    value = -1
    ios = -99
  end subroutine reset_values

end program ar44_namelist_quoted_slash
