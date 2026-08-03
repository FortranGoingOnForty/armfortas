! List-directed character input recognizes both delimiters, keeps value
! separators inside quotes, removes the delimiters, and collapses doubled
! delimiters without making quoted numeric input valid.
!
! FLAGS: --std=f2023
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|repro
! IR_CHECK: call @afs_read_string
! IR_CHECK: call @afs_read_internal_string
program ar44_list_directed_character_quotes
  implicit none

  character(len=16) :: external_values(6)
  character(len=16) :: internal_values(3)
  character(len=16) :: escaped_double
  character(len=96) :: record
  integer :: unit
  integer :: ios
  integer :: number

  open(newunit=unit, status='scratch', action='readwrite', form='formatted')
  write(unit, '(A)') "'alpha beta', ""gamma,delta"", 'don''t', ' spaced ', '', plain"
  rewind(unit)
  external_values = 'sentinel'
  read(unit, *, iostat=ios) external_values
  if (ios /= 0) error stop 1
  close(unit)

  if (external_values(1) /= 'alpha beta') error stop 2
  if (external_values(2) /= 'gamma,delta') error stop 3
  if (external_values(3) /= "don't") error stop 4
  if (external_values(4) /= ' spaced ') error stop 5
  if (len_trim(external_values(5)) /= 0) error stop 6
  if (external_values(6) /= 'plain') error stop 7

  record = "'left,right', ""two words"", 'say ''hello'''"
  internal_values = 'sentinel'
  read(record, *, iostat=ios) internal_values(1), internal_values(2), internal_values(3)
  if (ios /= 0) error stop 8
  if (internal_values(1) /= 'left,right') error stop 9
  if (internal_values(2) /= 'two words') error stop 10
  if (internal_values(3) /= "say 'hello'") error stop 11

  record = '"say ""hi"""'
  escaped_double = 'sentinel'
  read(record, *, iostat=ios) escaped_double
  if (ios /= 0) error stop 12
  if (escaped_double /= 'say "hi"') error stop 13

  record = "'42'"
  number = -1
  read(record, *, iostat=ios) number
  if (ios == 0) error stop 14
  if (number /= -1) error stop 15

  print '(a)', 'ok'
end program ar44_list_directed_character_quotes
