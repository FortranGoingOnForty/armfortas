! Character intrinsics associate keyword actuals by their standard dummy names,
! preserve explicit INTEGER result kinds, normalize every supported LOGICAL
! carrier used for BACK, and evaluate owned/side-effecting actuals exactly once.
!
! FLAGS: --std=f2018
! CHECK: ok
! REPRO_CHECK: asm
! REPRO_CHECK: obj
! REPRO_CHECK: run
! OPT_EQ: O0,O1,O2,O3,Os,Ofast => stdout|stderr|exit
! PHASE_TRIANGULATE: ir|asm|obj|clean|repro
! IR_CHECK: call @afs_index
! IR_CHECK: call @afs_scan
! IR_CHECK: call @afs_verify
! IR_CHECK: call @afs_ichar_ptr
! IR_CHECK: call @afs_repeat
program ar56_character_intrinsic_keywords
  use iso_fortran_env, only: int8, int16, int32, int64
  implicit none

  integer :: calls
  logical(int8) :: back1
  logical(int64) :: back8
  character(8) :: text

  calls = 0
  back1 = .true.
  back8 = .true.

  if (index(kind=int64, substring='na', string='banana') /= 3_int64) error stop 1
  if (scan(kind=int16, back=back1, set='ab', string='cabca') /= 5_int16) error stop 2
  if (verify(kind=int8, set='ab', string='abXba') /= 3_int8) error stop 3
  if (index(kind=16, back=back8, substring='na', string='banana') /= 5_16) error stop 4
  if (scan(kind=int32, set='xy', string='abc') /= 0_int32) error stop 5
  if (verify(kind=int64, back=.true., set='ab', string='abXba') /= 3_int64) error stop 6

  if (len(kind=int64, string='abcd') /= 4_int64) error stop 7
  if (len_trim(kind=int16, string='ab  ') /= 2_int16) error stop 8
  if (ichar(kind=int64, c='A') /= 65_int64) error stop 9
  if (iachar(kind=int8, c='B') /= 66_int8) error stop 10
  if (len(kind=kind(0_int64), string='abcd') /= 4_int64) error stop 28
  if (index(kind=selected_int_kind(18), substring='na', string='banana') /= 3_int64) &
      error stop 29
  if (verify(kind=selected_real_kind(r=300, p=15), set='ab', string='abXba') /= 3_int64) &
      error stop 30
  if (ichar(kind=selected_char_kind('ascii'), c='A') /= 65_1) error stop 31
  if (kind(index(string='a', substring='a')) /= kind(0)) error stop 11
  if (kind(len(string='a')) /= kind(0)) error stop 12
  if (kind(ichar(c='a')) /= kind(0)) error stop 13

  text = repeat(ncopies=3, string='xy')
  if (text /= 'xyxyxy  ') error stop 14
  text = adjustl(string='  xy    ')
  if (text /= 'xy      ') error stop 15
  text = adjustr(string='xy      ')
  if (text /= '      xy') error stop 16
  text = trim(string='xy      ')
  if (text /= 'xy      ') error stop 17
  if (char(kind=1, i=65) /= 'A') error stop 18
  if (achar(kind=1, i=66) /= 'B') error stop 19
  if (new_line(a='x') /= achar(10)) error stop 20

  if (.not. lge(string_b='A', string_a='B')) error stop 21
  if (.not. lgt(string_b='A', string_a='B')) error stop 22
  if (.not. lle(string_b='B', string_a='A')) error stop 23
  if (.not. llt(string_b='B', string_a='A')) error stop 24

  if (index(substring='na', string=make_text()) /= 3) error stop 25
  if (calls /= 1) error stop 26
  if (index(substring=trim(string='na  '), string=repeat(ncopies=3, string='na')) /= 1) &
      error stop 27

  print '(a)', 'ok'

contains

  function make_text() result(result)
    character(6) :: result
    calls = calls + 1
    result = 'banana'
  end function make_text

end program ar56_character_intrinsic_keywords
